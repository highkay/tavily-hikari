// ===========================================================================
// Zhihu LOCAL MCP facade (`/zhihu/mcp`) — keenable-integration plan todo 10.
//
// The official Zhihu MCP is SSE-only, so this surface is a Hikari-LOCAL facade:
// initialize/tools-list/ping/notifications answer LOCALLY (session-less,
// SSE-wrapped, server_name `zhihu-search-hikari`, EXACTLY the two SPEC catalog
// tools) and tools/call translates into a pooled-key Zhihu REST GET (Bearer
// Access Secret + fresh server-side X-Request-Timestamp). Billing settles
// 1 credit ONLY when the translated envelope carries `Code == 0`; a non-zero
// `Code` (always HTTP 200) surfaces as an MCP tool error at zero credits with
// no key recovery from the MCP layer. Ingress rules: a GET opening an event
// stream → 405, an unknown tool → 400, a Tavily session id → 404
// session_unavailable, and the Access Secret never appears in a result or log.
//
// Real server binary + real mock_zhihu binary + direct sqlite assertions (the
// proven brave/exa-section pattern). Every upstream is a stub — production
// endpoints are never contacted.
// ===========================================================================



/// Criterion (plan todo 10 catalog): initialize then tools/list shows EXACTLY
/// the two SPEC catalog tools, each with an object inputSchema; session-less
/// (no mcp-session-id minted), non-billable (zero ledger), zero upstream calls.
#[tokio::test]
async fn zhihu_initialize_and_tools_list_advertise_exactly_two_tools() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-catalog", &["mock-zhihu-key"]).await;

    let init = harness.initialize(1).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"].as_str(),
        Some("zhihu-search-hikari"),
        "session-less local serverInfo: {init}"
    );

    let catalog = harness
        .zhihu_mcp_json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .await;
    let names: Vec<String> = catalog["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        names,
        ["zhihu_search", "zhihu_global_search"],
        "EXACTLY the two SPEC catalog tools: {catalog}"
    );
    for tool in catalog["result"]["tools"].as_array().expect("tools array") {
        assert_eq!(tool["inputSchema"]["type"], json!("object"));
        assert_eq!(
            tool["inputSchema"]["required"],
            json!(["query"]),
            "query is the single required field"
        );
    }

    // Non-billable control plane: zero ledger rows, zero upstream calls.
    assert!(harness.charged_ledger_rows().await.is_empty());
    assert_eq!(
        harness.mock_metrics().await["zhihu_search"],
        json!(0),
        "tools/list never reaches the zhihu upstream"
    );
}

/// Criterion (plan todo 10 happy path): tools/call zhihu_search count=3 with a
/// `Code=0` envelope settles EXACTLY 1 credit, stamps the
/// `mcp:zhihu/zhihu_search` request kind on a provider-stamped token log,
/// returns the native envelope as the SSE tool-result content, and the wire GET
/// carries the translated params plus the pooled Bearer key and a fresh
/// server-side timestamp (never the client Authorization).
#[tokio::test]
async fn zhihu_search_bills_one_credit_with_zhihu_kind_and_translated_params() {
    let pool_key = "mock-zhihu-key";
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-search", &[pool_key]).await;
    harness.initialize(1).await;

    let result = harness
        .zhihu_mcp_json(&ZhihuMcpHarness::tools_call(
            2,
            "zhihu_search",
            json!({ "query": "count test", "count": 3, "sort_by": "VoteUpCount:desc" }),
        ))
        .await;
    assert_eq!(result["id"], json!(2));
    assert!(
        result["result"].get("isError").is_none(),
        "Code=0 is a success result: {result}"
    );
    // The native {Code,Message,Data} envelope rides the tool result verbatim
    // (structuredContent + text), untouched beyond the envelope.
    assert_eq!(result["result"]["structuredContent"]["Code"], json!(0));
    assert_eq!(
        result["result"]["structuredContent"]["Data"]["Items"][0]["Title"],
        json!("Mock Zhihu result for count test"),
        "native body verbatim: {result}"
    );
    assert_eq!(
        result["result"]["structuredContent"]["Data"]["Items"][0]["RankingScore"],
        json!(1.25),
        "RankingScore is never stripped"
    );
    assert_eq!(
        result["result"]["structuredContent"]["Data"]["Items"][0]["CreateTime"],
        json!(1_788_278_525_u64),
        "ContentType/CreateTime are never stripped"
    );
    assert_eq!(
        result["result"]["content"][0]["type"], json!("text"),
        "text content carries the native envelope"
    );

    // Wire translation: Query/Count/SortBy on the REST route.
    let last = harness.mock_last_bodies().await;
    assert_eq!(
        last["zhihu_search"].as_str().expect("last query"),
        "Count=3&Query=count+test&SortBy=VoteUpCount%3Adesc",
        "translated REST query (JSON object keys iterate sorted): {last}"
    );
    // Pooled Bearer + fresh server-side timestamp; never the th- token.
    let metrics = harness.mock_metrics().await;
    assert_eq!(
        metrics["last_authorization"],
        json!(format!("Bearer {pool_key}")),
        "pooled Access Secret rides the Bearer slot: {metrics}"
    );
    let timestamp: i64 = metrics["last_x_request_timestamp"]
        .as_str()
        .expect("server-side timestamp")
        .parse()
        .expect("unix-second timestamp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    assert!(
        (now - 600..=now + 600).contains(&timestamp),
        "fresh server-side X-Request-Timestamp: {timestamp} vs {now}"
    );

    // Exactly 1 settled credit on the shared wallet.
    assert_eq!(harness.charged_ledger_rows().await, [("charged".to_string(), 1)]);
    // Token log carries the committed mcp:zhihu/zhihu_search kind + credit.
    let logs = harness.token_log_rows().await;
    let tool_rows: Vec<_> = logs
        .iter()
        .filter(|(kind, _, _)| kind.as_deref() == Some("mcp:zhihu/zhihu_search"))
        .collect();
    assert_eq!(tool_rows.len(), 1, "one mcp:zhihu/zhihu_search row: {logs:?}");
    assert_eq!(tool_rows[0].1, Some(1), "business credit column: {logs:?}");
    // Provider-stamped request_logs trail (local-facade settle keeps the
    // auth_token_logs provider column at the legacy NULL default).
    let (provider, kind, result_status) = harness.latest_request_log_row().await;
    assert_eq!(provider.as_deref(), Some("zhihu"), "provider stamp");
    assert_eq!(kind.as_deref(), Some("mcp:zhihu/zhihu_search"));
    assert_eq!(result_status, "success");
    // Redaction gate: the pooled Access Secret never lands in a persisted
    // request_logs body (the envelope and the JSON-RPC body are secret-free).
    assert!(
        !harness.request_log_bodies_leak_secret(pool_key).await,
        "the Access Secret must never leak into persisted logs"
    );
}

/// Criterion (plan todo 10 second tool): zhihu_global_search maps its args
/// onto the REST route (Count cap 20, SearchDB enum, Filter URL-encoding) and
/// settles 1 credit with the `mcp:zhihu/global_search` kind.
#[tokio::test]
async fn zhihu_global_search_maps_args_and_bills_one_credit() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-global", &["mock-zhihu-key"]).await;
    harness.initialize(1).await;

    let result = harness
        .zhihu_mcp_json(&ZhihuMcpHarness::tools_call(
            2,
            "zhihu_global_search",
            json!({
                "query": "global test",
                "count": 20,
                "search_db": "realtime",
                "filter": "host==\"example.com\" AND publish_time>=1600000000"
            }),
        ))
        .await;
    assert_eq!(result["result"]["structuredContent"]["Code"], json!(0));
    assert_eq!(
        result["result"]["structuredContent"]["Data"]["Items"][0]["Title"],
        json!("Mock Zhihu result for global test")
    );

    let last = harness.mock_last_bodies().await;
    assert_eq!(
        last["global_search"].as_str().expect("last query"),
        "Count=20&Filter=host%3D%3D%22example.com%22+AND+publish_time%3E%3D1600000000&Query=global+test&SearchDB=realtime",
        "translated REST query (JSON object keys iterate sorted): {last}"
    );

    assert_eq!(harness.charged_ledger_rows().await, [("charged".to_string(), 1)]);
    let logs = harness.token_log_rows().await;
    assert!(
        logs.iter()
            .any(|(kind, credits, _)| kind.as_deref() == Some("mcp:zhihu/global_search") && *credits == Some(1)),
        "one mcp:zhihu/global_search credited row: {logs:?}"
    );
    let (provider, kind, result_status) = harness.latest_request_log_row().await;
    assert_eq!(provider.as_deref(), Some("zhihu"), "provider stamp");
    assert_eq!(kind.as_deref(), Some("mcp:zhihu/global_search"));
    assert_eq!(result_status, "success");
}

/// Criterion (plan todo 10 failure path): a `Code=20001` auth-failure envelope
/// (HTTP 200 at the transport) is an MCP TOOL ERROR with ZERO credits and NO
/// key recovery from the MCP layer (single attempt; the todo-9 record path
/// owns the quarantine).
#[tokio::test]
async fn zhihu_auth_failure_envelope_is_tool_error_and_bills_zero() {
    // tok-auth: the mock answers every content call with Code=20001.
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-authfail", &["tok-auth"]).await;
    harness.initialize(1).await;

    let result = harness
        .zhihu_mcp_call(
            None,
            &ZhihuMcpHarness::tools_call(2, "zhihu_search", json!({ "query": "auth", "count": 3 })),
        )
        .await;
    assert_eq!(result.status, StatusCode::OK, "envelope arrives at HTTP 200");
    let payload = parse_sse_json(&result.body);
    assert_eq!(payload["result"]["isError"], json!(true), "MCP tool error");
    assert_eq!(
        payload["result"]["structuredContent"]["Code"],
        json!(20001),
        "native envelope rides the error verbatim: {payload}"
    );

    // Zero credits, zero charged ledger.
    assert!(harness.charged_ledger_rows().await.is_empty());
    let metrics = harness.mock_metrics().await;
    assert_eq!(
        metrics["zhihu_search"], 1,
        "SINGLE attempt — no key recovery from the MCP layer: {metrics}"
    );
    // The todo-9 record path quarantined the dead Access Secret (single
    // attempt — the MCP layer itself never recovers the key).
    assert_eq!(
        harness.active_quarantine_count("tok-auth").await,
        1,
        "Code=20001 quarantines the pooled secret"
    );
}

/// Criterion (plan todo 10 ingress rules): an unknown tool — including the
/// SPEC out-of-scope native tools — is a typed 400 with ZERO upstream calls and
/// ZERO billing (the SPEC catalog is exactly the two tools).
#[tokio::test]
async fn zhihu_unknown_tool_is_a_typed_400_without_upstream_call() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-unknown", &["mock-zhihu-key"]).await;
    harness.initialize(1).await;

    for (id, tool) in [(2u64, "hot_list"), (3u64, "zhihu_answer")] {
        let result = harness
            .zhihu_mcp_call(None, &ZhihuMcpHarness::tools_call(id, tool, json!({ "query": "x" })))
            .await;
        assert_eq!(result.status, StatusCode::BAD_REQUEST, "{tool} unknown-tool 400");
        let payload = parse_sse_json(&result.body);
        assert_eq!(payload["result"]["isError"], json!(true), "{tool} isError");
        let content_text = payload["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            content_text.contains("Unknown tool"),
            "typed unknown-tool error for {tool}: {content_text}"
        );
    }

    assert_eq!(
        harness.mock_metrics().await["zhihu_search"],
        json!(0),
        "unknown tools never reach the zhihu upstream"
    );
    assert!(harness.charged_ledger_rows().await.is_empty());
}

/// Criterion (plan todo 10 QA failure scenario): `count=11` on zhihu_search is
/// a TYPED local validation error with ZERO upstream calls and ZERO billing.
#[tokio::test]
async fn zhihu_count_over_cap_is_a_local_validation_error_without_upstream() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-count-cap", &["mock-zhihu-key"]).await;
    harness.initialize(1).await;

    let result = harness
        .zhihu_mcp_call(
            None,
            &ZhihuMcpHarness::tools_call(2, "zhihu_search", json!({ "query": "cap", "count": 11 })),
        )
        .await;
    let payload = parse_sse_json(&result.body);
    assert_eq!(payload["result"]["isError"], json!(true), "typed validation error");
    let content_text = payload["result"]["content"][0]["text"].as_str().expect("text");
    assert!(
        content_text.contains("count"),
        "validation error names the count bound: {content_text}"
    );

    assert_eq!(
        harness.mock_metrics().await["zhihu_search"],
        json!(0),
        "local validation rejects before ANY upstream call"
    );
    assert!(harness.charged_ledger_rows().await.is_empty());
}

/// Criterion (plan todo 10 ingress rules): a GET opening an event stream is
/// rejected locally with 405 (the facade buffers responses; the official
/// SSE-only Zhihu MCP is deliberately NOT used).
#[tokio::test]
async fn zhihu_get_event_stream_opener_is_405() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-get-405", &["mock-zhihu-key"]).await;

    let response = Client::new()
        .get(format!("{}/zhihu/mcp", harness.base_url))
        .bearer_auth(&harness.token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("GET event-stream opener");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

    assert_eq!(
        harness.mock_metrics().await["zhihu_search"],
        json!(0),
        "the 405 never dials the upstream"
    );
}

/// Criterion (plan todo 10 ingress rules): a Tavily session id (an ACTIVE
/// provider-untagged control session on the SAME token) can never resolve on
/// the session-less zhihu facade — typed 404 session_unavailable, zero upstream
/// calls, zero billing.
#[tokio::test]
async fn zhihu_rejects_a_tavily_session_id() {
    let harness = spawn_zhihu_mcp_harness("mcp-e2e-zhihu-tavily-session", &["mock-zhihu-key"]).await;
    let tavily_session = "tavily-control-session-for-zhihu-reject";
    harness.seed_tavily_control_session(tavily_session).await;

    let response = Client::new()
        .post(format!("{}/zhihu/mcp", harness.base_url))
        .bearer_auth(&harness.token)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", tavily_session)
        .json(&ZhihuMcpHarness::tools_call(
            1,
            "zhihu_search",
            json!({ "query": "session", "count": 3 }),
        ))
        .send()
        .await
        .expect("zhihu mcp request with tavily session id");
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "session id rejected");
    let body: Value = response.json().await.expect("json body");
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("session_unavailable"),
        "typed session_unavailable vocabulary: {body}"
    );

    assert_eq!(
        harness.mock_metrics().await["zhihu_search"],
        json!(0),
        "the rejected session never reaches the upstream"
    );
    assert!(harness.charged_ledger_rows().await.is_empty());
}
