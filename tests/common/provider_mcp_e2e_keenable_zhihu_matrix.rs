// ---------------------------------------------------------------------------
// Keenable + Zhihu cross-surface MCP hardening matrix (keenable-integration
// plan todo 20). Both provider MCP surfaces live in ONE server instance
// (mock_keenable + mock_zhihu both live), so these cells express what the
// per-surface todo-8/todo-10 sections cannot:
//
// - one shared wallet billing across BOTH MCP surfaces with byte-stable
//   upstream isolation (each mock sees only its own surface and only its own
//   pooled credential in its own native slot);
// - proxy session ids never crossing the two surfaces in either direction;
// - the full control plane (initialize / notifications / ping / tools-list)
//   staying free on BOTH surfaces with zero ledger rows and zero billable
//   upstream calls.
//
// Included at the crate-root scope of tests/provider_mcp_e2e.rs (the
// serpapi/keenable/zhihu harness include precedent): every upstream is a
// stub; production endpoints are never contacted.
// ---------------------------------------------------------------------------

struct KeenableZhihuMcpHarness {
    base_url: String,
    keenable_mock_base: String,
    zhihu_mock_base: String,
    token: String,
    token_id: String,
    db_path: PathBuf,
    db_str: String,
    _server: ChildGuard,
    _keenable_mock: ChildGuard,
    _zhihu_mock: ChildGuard,
}

impl Drop for KeenableZhihuMcpHarness {
    fn drop(&mut self) {
        remove_sqlite_files(&self.db_path);
    }
}

/// Spawn mock_keenable + mock_zhihu + the real server binary with BOTH
/// provider MCP surfaces live; every other provider upstream (Tavily
/// included) points at a dead local port (stub-only rule by construction).
async fn spawn_keenable_zhihu_mcp_harness(
    prefix: &str,
    keenable_pool: &[&str],
    keenable_mock_keys: &[&str],
    zhihu_pool: &[&str],
    zhihu_mock_keys: &[&str],
) -> KeenableZhihuMcpHarness {
    for attempt in 1..=5usize {
        if let Some(harness) = try_spawn_keenable_zhihu_mcp_harness(
            prefix,
            keenable_pool,
            keenable_mock_keys,
            zhihu_pool,
            zhihu_mock_keys,
        )
        .await
        {
            return harness;
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    panic!("keenable+zhihu mcp harness never became healthy after 5 spawn attempts");
}

async fn try_spawn_keenable_zhihu_mcp_harness(
    prefix: &str,
    keenable_pool: &[&str],
    keenable_mock_keys: &[&str],
    zhihu_pool: &[&str],
    zhihu_mock_keys: &[&str],
) -> Option<KeenableZhihuMcpHarness> {
    let keenable_port = reserve_local_port();
    let keenable_mock = Command::new(support_binaries::mock_keenable_binary())
        .arg("--bind")
        .arg(format!("127.0.0.1:{keenable_port}"))
        .arg("--keys")
        .arg(keenable_mock_keys.join(","))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mock_keenable");

    let zhihu_port = reserve_local_port();
    let zhihu_mock = Command::new(support_binaries::mock_zhihu_binary())
        .arg("--bind")
        .arg(format!("127.0.0.1:{zhihu_port}"))
        .arg("--tokens")
        .arg(zhihu_mock_keys.join(","))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mock_zhihu");

    let port = reserve_local_port();
    let db_path = temp_db_path(prefix);
    let db_str = db_path.to_string_lossy().to_string();
    let dead_port = reserve_local_port();
    let server = Command::new(support_binaries::resolve_support_binary(
        "TAVILY_HIKARI_TEST_BIN",
        env!("CARGO_BIN_EXE_tavily-hikari"),
    ))
    .env("TAVILY_API_KEYS", "tvly-mcp-ke-zh-matrix-key")
    .env(
        "TAVILY_UPSTREAM",
        format!("http://127.0.0.1:{dead_port}/mcp"),
    )
    .env("TAVILY_USAGE_BASE", format!("http://127.0.0.1:{dead_port}"))
    .env("SERPAPI_UPSTREAM", format!("http://127.0.0.1:{dead_port}"))
    .env(
        "SERPAPI_MCP_UPSTREAM",
        format!("http://127.0.0.1:{dead_port}/mcp"),
    )
    .env("EXA_UPSTREAM", format!("http://127.0.0.1:{dead_port}"))
    .env("BRAVE_UPSTREAM", format!("http://127.0.0.1:{dead_port}"))
    .env(
        "KEENABLE_UPSTREAM",
        format!("http://127.0.0.1:{keenable_port}"),
    )
    .env(
        "KEENABLE_MCP_UPSTREAM",
        format!("http://127.0.0.1:{keenable_port}/mcp"),
    )
    .env("KEENABLE_API_KEYS", keenable_pool.join(","))
    .env("ZHIHU_UPSTREAM", format!("http://127.0.0.1:{zhihu_port}"))
    .env("ZHIHU_API_KEYS", zhihu_pool.join(","))
    .env("TAVILY_DISABLE_BACKGROUND_TASKS", "1")
    .env("PROXY_BIND", "127.0.0.1")
    .env("PROXY_PORT", port.to_string())
    .env("PROXY_DB_PATH", &db_str)
    .env("DEV_OPEN_ADMIN", "true")
    .env("RUST_LOG", "warn,tavily_hikari=info,sqlx::query=error")
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .expect("spawn tavily-hikari");

    if !health_ready_within(port, 25).await {
        drop(ChildGuard { child: server });
        drop(ChildGuard {
            child: keenable_mock,
        });
        drop(ChildGuard { child: zhihu_mock });
        remove_sqlite_files(&db_path);
        return None;
    }
    wait_for_provider_keys(
        &db_str,
        "keenable",
        i64::try_from(keenable_pool.len()).unwrap(),
    )
    .await;
    wait_for_provider_keys(
        &db_str,
        "zhihu",
        i64::try_from(zhihu_pool.len()).unwrap(),
    )
    .await;
    let base_url = format!("http://127.0.0.1:{port}");
    let (token, token_id) = create_test_token(&base_url).await;

    Some(KeenableZhihuMcpHarness {
        base_url,
        keenable_mock_base: format!("http://127.0.0.1:{keenable_port}"),
        zhihu_mock_base: format!("http://127.0.0.1:{zhihu_port}"),
        token,
        token_id,
        db_path,
        db_str,
        _server: ChildGuard { child: server },
        _keenable_mock: ChildGuard {
            child: keenable_mock,
        },
        _zhihu_mock: ChildGuard {
            child: zhihu_mock,
        },
    })
}

impl KeenableZhihuMcpHarness {
    async fn pool(&self) -> sqlx::SqlitePool {
        connect_sqlite_test_pool(&self.db_str).await
    }

    /// POST one JSON-RPC message to either provider MCP ingress with the
    /// Hikari Bearer token and (optionally) a proxy mcp-session-id header.
    async fn mcp_call(
        &self,
        path: &str,
        session_id: Option<&str>,
        message: &Value,
    ) -> McpCallResult {
        let mut request = Client::new()
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id);
        }
        let response = request.json(message).send().await.expect("mcp request");
        McpCallResult {
            status: response.status(),
            session_header: response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            content_type: response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body: response.bytes().await.expect("response body").to_vec(),
        }
    }

    fn initialize_message(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "ke-zh-mcp-matrix", "version": "0.1.0" }
            }
        })
    }

    fn tools_call_message(id: u64, tool: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        })
    }

    /// Run initialize on `/keenable/mcp` and return the translated proxy
    /// session id (notifications/initialized completes the handshake).
    async fn keenable_initialize_session(&self, id: u64) -> String {
        let response = self
            .mcp_call("/keenable/mcp", None, &Self::initialize_message(id))
            .await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "keenable initialize: {}",
            response.body_text()
        );
        let session = response
            .session_header
            .clone()
            .expect("initialize returns the translated proxy mcp-session-id");
        let notified = self
            .mcp_call(
                "/keenable/mcp",
                Some(&session),
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            )
            .await;
        assert_eq!(notified.status, StatusCode::ACCEPTED, "notification 202");
        session
    }

    async fn keenable_mock_metrics(&self) -> Value {
        Client::new()
            .get(format!("{}/metrics", self.keenable_mock_base))
            .send()
            .await
            .expect("keenable mock metrics request")
            .json::<Value>()
            .await
            .expect("keenable mock metrics body")
    }

    async fn zhihu_mock_metrics(&self) -> Value {
        Client::new()
            .get(format!("{}/metrics", self.zhihu_mock_base))
            .send()
            .await
            .expect("zhihu mock metrics request")
            .json::<Value>()
            .await
            .expect("zhihu mock metrics body")
    }

    /// (billing_state, business_credits) for every ledger row on the token.
    async fn charged_ledger_rows(&self) -> Vec<(String, i64)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT billing_state, business_credits FROM billing_ledger \
             WHERE token_id = ? ORDER BY auth_token_log_id ASC",
        )
        .bind(&self.token_id)
        .fetch_all(&pool)
        .await
        .expect("ledger rows");
        pool.close().await;
        rows.into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("billing_state"),
                    row.get::<i64, _>("business_credits"),
                )
            })
            .collect()
    }

    /// (request_kind_key, business_credits, provider) per auth_token_logs row.
    async fn token_log_rows(&self) -> Vec<(Option<String>, Option<i64>, Option<String>)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT request_kind_key, business_credits, provider FROM auth_token_logs \
             WHERE token_id = ? ORDER BY id ASC",
        )
        .bind(&self.token_id)
        .fetch_all(&pool)
        .await
        .expect("token log rows");
        pool.close().await;
        rows.into_iter()
            .map(|row| {
                (
                    row.get::<Option<String>, _>("request_kind_key"),
                    row.get::<Option<i64>, _>("business_credits"),
                    row.get::<Option<String>, _>("provider"),
                )
            })
            .collect()
    }

    /// Seed an ACTIVE zhihu-provider MCP session row bound to this token, so
    /// a real zhihu-stamped session id can be presented on the keenable
    /// surface (the session-crossing mirror of the tavily/serpapi rejections).
    async fn seed_zhihu_session(&self, proxy_session_id: &str) {
        let pool = self.pool().await;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO mcp_sessions \
             (proxy_session_id, auth_token_id, gateway_mode, experiment_variant, provider, \
              created_at, updated_at, expires_at) \
             VALUES (?, ?, 'upstream_mcp', 'control', 'zhihu', ?, ?, ?)",
        )
        .bind(proxy_session_id)
        .bind(&self.token_id)
        .bind(now)
        .bind(now)
        .bind(now + 3600)
        .execute(&pool)
        .await
        .expect("seed zhihu session row");
        pool.close().await;
    }
}

/// One shared wallet across BOTH MCP surfaces (todo 20): initialize + a
/// tools/call on `/keenable/mcp` (1 credit) and initialize + a tools/call on
/// `/zhihu/mcp` (1 credit) settle exactly two charged rows with the two
/// provider-scoped kinds on one monthly counter, while each mock observes
/// ONLY its own surface with ONLY its own pooled credential in its own
/// native slot (X-API-Key for keenable — never an Authorization header;
/// Bearer Access Secret for zhihu).
#[tokio::test]
async fn keenable_and_zhihu_mcp_surfaces_share_one_wallet_and_stay_isolated() {
    let harness = spawn_keenable_zhihu_mcp_harness(
        "mcp-ke-zh-wallet",
        &["ke-mcp-1"],
        &["ke-mcp-1"],
        &["tok-mcp-1"],
        &["tok-mcp-1"],
    )
    .await;

    // Keenable leg: initialize (free) then a billable search_web_pages call.
    let session = harness.keenable_initialize_session(1).await;
    let response = harness
        .mcp_call(
            "/keenable/mcp",
            Some(&session),
            &KeenableZhihuMcpHarness::tools_call_message(
                2,
                "search_web_pages",
                json!({ "query": "shared wallet", "mode": "pro" }),
            ),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "keenable tools/call: {}",
        response.body_text()
    );
    let body = response.json();
    assert_eq!(body["result"]["_meta"]["keenable/usage"]["credits_remaining"], json!(99_999));

    // Zhihu leg: initialize (free, session-less local) then a billable
    // zhihu_search call.
    let zhihu_init = harness
        .mcp_call("/zhihu/mcp", None, &KeenableZhihuMcpHarness::initialize_message(3))
        .await;
    assert_eq!(zhihu_init.status, StatusCode::OK, "zhihu initialize");
    assert!(
        zhihu_init.session_header.is_none(),
        "the session-less zhihu facade never mints a session id"
    );
    let response = harness
        .mcp_call(
            "/zhihu/mcp",
            None,
            &KeenableZhihuMcpHarness::tools_call_message(
                4,
                "zhihu_search",
                json!({ "query": "shared wallet", "count": 3 }),
            ),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "zhihu tools/call: {}",
        response.body_text()
    );
    let payload = parse_sse_json(&response.body);
    assert_eq!(
        payload["result"]["structuredContent"]["Code"],
        json!(0),
        "the native envelope rides the SSE tool result: {payload}"
    );

    // ONE shared wallet: exactly two charged rows, one credit each, and the
    // provider-scoped kinds on the token logs.
    assert_eq!(
        harness.charged_ledger_rows().await,
        [("charged".to_string(), 1), ("charged".to_string(), 1)]
    );
    let logs = harness.token_log_rows().await;
    let billable: Vec<_> = logs
        .iter()
        .filter(|(_, credits, _)| credits.unwrap_or(0) > 0)
        .collect();
    assert_eq!(billable.len(), 2, "exactly two credited rows: {logs:?}");
    // (auth_token_logs keeps the legacy NULL provider default — the kind key
    // and the linked request_logs row carry the provider dimension.)
    assert!(
        billable
            .iter()
            .any(|(kind, credits, _)| kind.as_deref() == Some("mcp:keenable/search_web_pages")
                && *credits == Some(1)),
        "the keenable tools/call settled its credit: {logs:?}"
    );
    assert!(
        billable
            .iter()
            .any(|(kind, credits, _)| kind.as_deref() == Some("mcp:zhihu/zhihu_search")
                && *credits == Some(1)),
        "the zhihu tools/call settled its credit: {logs:?}"
    );
    let pool = harness.pool().await;
    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT COALESCE(r.provider, '') FROM auth_token_logs l \
         JOIN observability.request_logs r ON r.id = l.request_log_id \
         WHERE l.token_id = ? AND l.business_credits > 0 ORDER BY l.id ASC",
    )
    .bind(&harness.token_id)
    .fetch_all(&pool)
    .await
    .expect("billed provider rows");
    assert_eq!(
        providers,
        vec!["keenable".to_string(), "zhihu".to_string()],
        "the billed rows carry the provider dimension on request_logs"
    );
    let pool = harness.pool().await;
    let month_count: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT month_count FROM auth_token_quota WHERE token_id = ? LIMIT 1), 0)",
    )
    .bind(&harness.token_id)
    .fetch_one(&pool)
    .await
    .expect("token quota row");
    assert_eq!(month_count, 2, "both MCP surfaces spend one wallet");
    pool.close().await;

    // Isolation: each mock saw ONLY its own surface with its own credential.
    let keenable_metrics = harness.keenable_mock_metrics().await;
    assert_eq!(keenable_metrics["mcp_tools_call"], json!(1));
    assert_eq!(keenable_metrics["last_api_key"], json!("ke-mcp-1"));
    assert_eq!(
        keenable_metrics["last_authorization"],
        Value::Null,
        "the keenable passthrough never sends an Authorization header: {keenable_metrics}"
    );
    let zhihu_metrics = harness.zhihu_mock_metrics().await;
    assert_eq!(zhihu_metrics["zhihu_search"], json!(1));
    assert_eq!(zhihu_metrics["global_search"], json!(0));
    assert_eq!(zhihu_metrics["last_authorization"], json!("Bearer tok-mcp-1"));
    assert_eq!(keenable_metrics["auth_failures"], json!(0));
    assert_eq!(zhihu_metrics["auth_failures"], json!(0));
}

/// Proxy session ids never cross the two surfaces (todo 20): a REAL keenable
/// proxy session minted on `/keenable/mcp` never resolves on `/zhihu/mcp`
/// (404 session_unavailable, zero upstream calls, zero billing), and a REAL
/// zhihu-provider session row never resolves on `/keenable/mcp` either —
/// after both rejections each surface still initializes its own session.
#[tokio::test]
async fn keenable_and_zhihu_mcp_sessions_never_cross_surfaces() {
    let harness = spawn_keenable_zhihu_mcp_harness(
        "mcp-ke-zh-cross",
        &["ke-mcp-2"],
        &["ke-mcp-2"],
        &["tok-mcp-2"],
        &["tok-mcp-2"],
    )
    .await;

    // Mint a REAL keenable session on this token.
    let keenable_session = harness.keenable_initialize_session(1).await;
    let keenable_before = harness.keenable_mock_metrics().await;

    // It never resolves on the zhihu surface.
    let crossed = harness
        .mcp_call(
            "/zhihu/mcp",
            Some(&keenable_session),
            &KeenableZhihuMcpHarness::tools_call_message(
                2,
                "zhihu_search",
                json!({ "query": "cross", "count": 3 }),
            ),
        )
        .await;
    assert_eq!(
        crossed.status,
        StatusCode::NOT_FOUND,
        "a keenable session must not resolve on /zhihu/mcp: {}",
        crossed.body_text()
    );
    let body: Value = serde_json::from_slice(&crossed.body).expect("json error body");
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("session_unavailable")
    );
    assert_eq!(
        harness.zhihu_mock_metrics().await["zhihu_search"],
        json!(0),
        "the rejected session never dials the zhihu upstream"
    );
    assert!(harness.charged_ledger_rows().await.is_empty());

    // Reverse direction: a REAL zhihu-provider session row never resolves on
    // the keenable surface.
    let zhihu_session = "zhihu-provider-session-for-keenable-reject";
    harness.seed_zhihu_session(zhihu_session).await;
    let crossed = harness
        .mcp_call(
            "/keenable/mcp",
            Some(zhihu_session),
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
        )
        .await;
    assert_eq!(
        crossed.status,
        StatusCode::NOT_FOUND,
        "a zhihu session must not resolve on /keenable/mcp: {}",
        crossed.body_text()
    );
    let body: Value = serde_json::from_slice(&crossed.body).expect("json error body");
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("session_unavailable")
    );
    assert_eq!(
        harness.keenable_mock_metrics().await["mcp"],
        keenable_before["mcp"],
        "the rejected session never dials the keenable upstream again"
    );

    // Both surfaces still initialize their own sessions afterwards.
    let fresh = harness.keenable_initialize_session(4).await;
    assert_ne!(fresh, keenable_session);
    let pool = harness.pool().await;
    let provider: Option<Option<String>> =
        sqlx::query_scalar("SELECT provider FROM mcp_sessions WHERE proxy_session_id = ?")
            .bind(&fresh)
            .fetch_one(&pool)
            .await
            .expect("fresh session row");
    assert_eq!(
        provider.unwrap().as_deref(),
        Some("keenable"),
        "the fresh session keeps its keenable provider stamp"
    );
    pool.close().await;
}

/// The full control plane stays free on BOTH surfaces (todo 20):
/// initialize / notifications/initialized / ping / tools-list on
/// `/keenable/mcp` and initialize / ping / tools-list on `/zhihu/mcp`
/// produce ZERO charged ledger rows, zero business credits on every token
/// log row, and zero billable upstream calls (the keenable control plane
/// passes through to its stub without ever hitting tools/call; the zhihu
/// control plane is answered entirely locally).
#[tokio::test]
async fn keenable_and_zhihu_mcp_control_plane_methods_bill_zero_on_both_surfaces() {
    let harness = spawn_keenable_zhihu_mcp_harness(
        "mcp-ke-zh-control",
        &["ke-mcp-3"],
        &["ke-mcp-3"],
        &["tok-mcp-3"],
        &["tok-mcp-3"],
    )
    .await;

    // Keenable control plane (notifications answer 202, the rest 200).
    let session = harness.keenable_initialize_session(1).await;
    for (message, expected) in [
        (
            json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }),
            StatusCode::OK,
        ),
        (
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }),
            StatusCode::OK,
        ),
        (
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {} }),
            StatusCode::ACCEPTED,
        ),
    ] {
        let response = harness
            .mcp_call("/keenable/mcp", Some(&session), &message)
            .await;
        assert_eq!(
            response.status,
            expected,
            "keenable control-plane method answers: {}",
            response.body_text()
        );
    }

    // Zhihu control plane (session-less local answers).
    for message in [
        KeenableZhihuMcpHarness::initialize_message(5),
        json!({ "jsonrpc": "2.0", "id": 6, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }),
    ] {
        let response = harness.mcp_call("/zhihu/mcp", None, &message).await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "zhihu control-plane method answers: {}",
            response.body_text()
        );
    }

    // Zero billing anywhere.
    assert!(
        harness.charged_ledger_rows().await.is_empty(),
        "control-plane methods never settle credits"
    );
    let logs = harness.token_log_rows().await;
    assert!(
        logs.iter().all(|(_, credits, _)| credits.unwrap_or(0) == 0),
        "no business credits on any control-plane row: {logs:?}"
    );

    // Zero billable upstream calls: the keenable stub saw control-plane
    // traffic but never a tools/call; the zhihu stub saw NOTHING.
    let keenable_metrics = harness.keenable_mock_metrics().await;
    assert_eq!(
        keenable_metrics["mcp_tools_call"],
        json!(0),
        "no keenable tools/call ever reached the stub: {keenable_metrics}"
    );
    assert_eq!(keenable_metrics["mcp_initialize"], json!(1));
    let zhihu_metrics = harness.zhihu_mock_metrics().await;
    assert_eq!(zhihu_metrics["zhihu_search"], json!(0));
    assert_eq!(zhihu_metrics["global_search"], json!(0));
    assert_eq!(zhihu_metrics["quota"], json!(0));
}
