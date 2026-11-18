// ---------------------------------------------------------------------------
// Keenable hosted MCP harness (real server binary + real mock_keenable
// binary) + the keenable-integration todo-8 acceptance tests.
//
// Extracted into tests/common/ for the 3050-line source budget of the
// provider_mcp_e2e target (serpapi-harness include precedent) and included
// back at the original position: same crate-root module scope, zero semantic
// or visibility changes.
// ---------------------------------------------------------------------------

struct KeenableMcpHarness {
    base_url: String,
    mock_base: String,
    token: String,
    token_id: String,
    db_path: PathBuf,
    db_str: String,
    /// Counters of the optional in-process Tavily-shaped mock upstream (the
    /// Tavily-session rejection test mints a real control session instead of
    /// hand-inserting a row).
    tavily_counters: Option<TavilyMockCounters>,
    _server: ChildGuard,
    _mock: ChildGuard,
}

impl Drop for KeenableMcpHarness {
    fn drop(&mut self) {
        remove_sqlite_files(&self.db_path);
    }
}

async fn spawn_keenable_harness(
    prefix: &str,
    pool_keys: &[&str],
    mock_keys: &[&str],
) -> KeenableMcpHarness {
    spawn_keenable_harness_with_serpapi_pool(prefix, pool_keys, mock_keys, &[], false).await
}

/// Spawn the harness with an additional SerpApi pool pointing at the SAME
/// mock (the mock accepts a `Bearer <key>` credential too): used by the
/// cross-surface session-isolation test, which mints a session on
/// `/serpapi/mcp` and presents it on `/keenable/mcp`. `tavily_mock` swaps the
/// dead Tavily upstream port for the in-process Tavily-shaped mock so a real
/// Tavily control session can be minted on `/mcp`.
async fn spawn_keenable_harness_with_serpapi_pool(
    prefix: &str,
    pool_keys: &[&str],
    mock_keys: &[&str],
    serpapi_pool_keys: &[&str],
    tavily_mock: bool,
) -> KeenableMcpHarness {
    for attempt in 1..=5usize {
        if let Some(harness) = try_spawn_keenable_harness(
            prefix,
            pool_keys,
            mock_keys,
            serpapi_pool_keys,
            tavily_mock,
        )
        .await
        {
            return harness;
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    panic!("keenable mcp harness never became healthy after 5 spawn attempts");
}

async fn try_spawn_keenable_harness(
    prefix: &str,
    pool_keys: &[&str],
    mock_keys: &[&str],
    serpapi_pool_keys: &[&str],
    tavily_mock: bool,
) -> Option<KeenableMcpHarness> {
    let mock_port = reserve_local_port();
    let mut mock_cmd = Command::new(support_binaries::mock_keenable_binary());
    mock_cmd
        .arg("--bind")
        .arg(format!("127.0.0.1:{mock_port}"))
        .arg("--keys")
        .arg(mock_keys.join(","))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mock = mock_cmd.spawn().expect("spawn mock_keenable");

    let port = reserve_local_port();
    let db_path = temp_db_path(prefix);
    let db_str = db_path.to_string_lossy().to_string();
    // Tavily surfaces must never contact production endpoints: default to a
    // dead local port; the Tavily-session rejection test swaps in the
    // in-process Tavily-shaped mock so a real control session can be minted.
    let dead_port = reserve_local_port();
    let (tavily_upstream, tavily_usage_base, tavily_counters) = if tavily_mock {
        let (upstream, counters) = spawn_tavily_mock_upstream().await;
        let base = upstream
            .strip_suffix("/mcp")
            .unwrap_or(&upstream)
            .to_string();
        (upstream, base, Some(counters))
    } else {
        (
            format!("http://127.0.0.1:{dead_port}/mcp"),
            format!("http://127.0.0.1:{dead_port}"),
            None,
        )
    };
    let mut server_cmd = Command::new(support_binaries::resolve_support_binary(
        "TAVILY_HIKARI_TEST_BIN",
        env!("CARGO_BIN_EXE_tavily-hikari"),
    ));
    server_cmd
        .env("TAVILY_API_KEYS", "tvly-keenable-mcp-e2e-key")
        .env("TAVILY_UPSTREAM", &tavily_upstream)
        .env("TAVILY_USAGE_BASE", &tavily_usage_base)
        .env(
            "KEENABLE_UPSTREAM",
            format!("http://127.0.0.1:{mock_port}"),
        )
        // Path suffix under the mock host: exercises the ingress-aware
        // upstream URL builder (KEENABLE_MCP_UPSTREAM honored with a /mcp
        // path).
        .env(
            "KEENABLE_MCP_UPSTREAM",
            format!("http://127.0.0.1:{mock_port}/mcp"),
        )
        .env("KEENABLE_API_KEYS", pool_keys.join(","))
        .env("PROXY_BIND", "127.0.0.1")
        .env("PROXY_PORT", port.to_string())
        .env("PROXY_DB_PATH", &db_str)
        .env("DEV_OPEN_ADMIN", "true")
        .env("RUST_LOG", "warn,tavily_hikari=info,sqlx::query=error");
    if !serpapi_pool_keys.is_empty() {
        // The same stub serves the SerpApi surface: the mock accepts the
        // Bearer credential form, so a serpapi session can be minted here
        // (stub-only rule: nothing dials production endpoints).
        server_cmd
            .env("SERPAPI_UPSTREAM", format!("http://127.0.0.1:{mock_port}"))
            .env(
                "SERPAPI_MCP_UPSTREAM",
                format!("http://127.0.0.1:{mock_port}/mcp"),
            )
            .env("SERPAPI_API_KEYS", serpapi_pool_keys.join(","));
    }
    let server = server_cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tavily-hikari");

    if !health_ready_within(port, 25).await {
        drop(ChildGuard { child: server });
        drop(ChildGuard { child: mock });
        remove_sqlite_files(&db_path);
        return None;
    }
    wait_for_provider_keys(
        &db_str,
        "keenable",
        i64::try_from(pool_keys.len()).unwrap(),
    )
    .await;
    if !serpapi_pool_keys.is_empty() {
        wait_for_provider_keys(
            &db_str,
            "serpapi",
            i64::try_from(serpapi_pool_keys.len()).unwrap(),
        )
        .await;
    }
    let base_url = format!("http://127.0.0.1:{port}");
    let (token, token_id) = create_test_token(&base_url).await;

    Some(KeenableMcpHarness {
        base_url,
        mock_base: format!("http://127.0.0.1:{mock_port}"),
        token,
        token_id,
        db_path,
        db_str,
        tavily_counters,
        _server: ChildGuard { child: server },
        _mock: ChildGuard { child: mock },
    })
}

impl KeenableMcpHarness {
    async fn pool(&self) -> sqlx::SqlitePool {
        connect_sqlite_test_pool(&self.db_str).await
    }

    /// POST one JSON-RPC message to a provider MCP ingress with the Hikari
    /// Bearer token and (optionally) the proxy mcp-session-id header.
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

    async fn keenable_mcp_call(&self, session_id: Option<&str>, message: &Value) -> McpCallResult {
        self.mcp_call("/keenable/mcp", session_id, message).await
    }

    fn initialize_message(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "provider-mcp-e2e", "version": "0.1.0" }
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
    /// session id; `notifications/initialized` completes the handshake.
    async fn initialize_session(&self, id: u64) -> String {
        let response = self
            .keenable_mcp_call(None, &Self::initialize_message(id))
            .await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "initialize must pass through: {}",
            response.body_text()
        );
        let session = response
            .session_header
            .clone()
            .expect("initialize must return the translated proxy mcp-session-id");
        assert_ne!(
            session, "mock-keenable-mcp-session",
            "client must never see the raw upstream session id"
        );
        let notified = self
            .keenable_mcp_call(
                Some(&session),
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            )
            .await;
        assert_eq!(notified.status, StatusCode::ACCEPTED, "notification 202");
        session
    }

    /// Snapshot of the mock's additive /metrics endpoint.
    async fn mock_metrics(&self) -> Value {
        Client::new()
            .get(format!("{}/metrics", self.mock_base))
            .send()
            .await
            .expect("mock metrics request")
            .json::<Value>()
            .await
            .expect("mock metrics json")
    }

    async fn mcp_session_row(&self, proxy_session_id: &str) -> sqlx::sqlite::SqliteRow {
        let pool = self.pool().await;
        let row = sqlx::query(
            "SELECT proxy_session_id, upstream_session_id, upstream_key_id, auth_token_id, \
             gateway_mode, experiment_variant, provider \
             FROM mcp_sessions WHERE proxy_session_id = ? LIMIT 1",
        )
        .bind(proxy_session_id)
        .fetch_one(&pool)
        .await
        .expect("mcp_sessions row for proxy session id");
        pool.close().await;
        row
    }

    /// Secret of the pooled key a proxy session is pinned to.
    async fn session_pinned_key_secret(&self, proxy_session_id: &str) -> String {
        let pool = self.pool().await;
        let secret: String = sqlx::query_scalar(
            "SELECT k.api_key FROM mcp_sessions s \
             JOIN api_keys k ON k.id = s.upstream_key_id \
             WHERE s.proxy_session_id = ? LIMIT 1",
        )
        .bind(proxy_session_id)
        .fetch_one(&pool)
        .await
        .expect("pinned key secret");
        pool.close().await;
        secret
    }

    async fn charged_ledger_rows(&self) -> Vec<(String, i64, Option<i64>)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT billing_state, business_credits, request_log_id FROM billing_ledger \
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
                    row.get::<Option<i64>, _>("request_log_id"),
                )
            })
            .collect()
    }

    /// The persisted passive X-RateLimit snapshot for one keenable key
    /// (`meta.keenable_rate_limit_v1:<key id>`), decoded (plan todo 22).
    async fn keenable_rate_limit_meta(&self, api_key: &str) -> Option<Value> {
        let pool = self.pool().await;
        let key_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM api_keys WHERE api_key = ? AND provider = 'keenable'",
        )
        .bind(api_key)
        .fetch_optional(&pool)
        .await
        .expect("keenable key id");
        let value = match key_id {
            Some(key_id) => sqlx::query_scalar::<_, String>(
                "SELECT value FROM meta WHERE key = ? LIMIT 1",
            )
            .bind(format!("keenable_rate_limit_v1:{key_id}"))
            .fetch_optional(&pool)
            .await
            .expect("rate limit meta row"),
            None => None,
        };
        pool.close().await;
        value.map(|text| serde_json::from_str(&text).expect("rate limit snapshot json"))
    }

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
}

// ---------------------------------------------------------------------------
// Acceptance tests (keenable-integration todo 8 criteria).
// ---------------------------------------------------------------------------

/// Criterion: initialize goes upstream, returns a TRANSLATED proxy session
/// id, persists an mcp_sessions row with provider='keenable' + upstream
/// identity + gateway_mode='upstream_mcp'; the pooled key rides `X-API-Key`
/// (NEVER Authorization — no Bearer assumption) and the request_logs row
/// carries the additive `keenable_mcp` upstream operation.
#[tokio::test]
async fn keenable_initialize_translates_session_and_tags_provider_row() {
    let harness = spawn_keenable_harness("mcp-ke-init", &["ke-pool-1"], &["ke-pool-1"]).await;
    let session = harness.initialize_session(1).await;

    let row = harness.mcp_session_row(&session).await;
    assert_eq!(row.get::<String, _>("proxy_session_id"), session);
    assert_eq!(
        row.get::<Option<String>, _>("provider").as_deref(),
        Some("keenable"),
        "mcp_sessions row must be provider-stamped"
    );
    assert_eq!(
        row.get::<Option<String>, _>("upstream_session_id").as_deref(),
        Some("mock-keenable-mcp-session"),
        "upstream session id translated and persisted"
    );
    assert_eq!(
        row.get::<String, _>("gateway_mode"),
        "upstream_mcp",
        "additive gateway mode value recorded"
    );
    assert_eq!(row.get::<String, _>("experiment_variant"), "control");
    assert_eq!(
        row.get::<Option<String>, _>("auth_token_id").as_deref(),
        Some(harness.token_id.as_str())
    );
    let upstream_key_id = row
        .get::<Option<String>, _>("upstream_key_id")
        .expect("pinned upstream key id");
    let pool = harness.pool().await;
    let (key_provider, key_secret): (String, String) =
        sqlx::query_as("SELECT provider, api_key FROM api_keys WHERE id = ?")
            .bind(&upstream_key_id)
            .fetch_one(&pool)
            .await
            .expect("pinned key row");
    assert_eq!(key_provider, "keenable");
    assert_eq!(key_secret, "ke-pool-1");

    // request_logs stamping: provider + gateway_mode + additive operation.
    let log = sqlx::query(
        "SELECT provider, gateway_mode, upstream_operation, proxy_session_id, request_kind_key \
         FROM observability.request_logs WHERE path = '/keenable/mcp' ORDER BY id ASC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("request log row");
    assert_eq!(
        log.get::<Option<String>, _>("provider").as_deref(),
        Some("keenable")
    );
    assert_eq!(
        log.get::<Option<String>, _>("gateway_mode").as_deref(),
        Some("upstream_mcp")
    );
    assert_eq!(
        log.get::<Option<String>, _>("upstream_operation").as_deref(),
        Some("keenable_mcp"),
        "additive provider-scoped upstream_operation value"
    );
    assert_eq!(
        log.get::<Option<String>, _>("proxy_session_id").as_deref(),
        Some(session.as_str())
    );
    assert_eq!(
        log.get::<Option<String>, _>("request_kind_key").as_deref(),
        Some("mcp:initialize")
    );

    // Mock-side proof: the pooled key rides the native X-API-Key slot, NO
    // Authorization header is ever sent upstream, and the mock saw exactly
    // one initialize.
    let metrics = harness.mock_metrics().await;
    assert_eq!(
        metrics["last_api_key"],
        json!("ke-pool-1"),
        "pooled key rides X-API-Key"
    );
    assert_eq!(
        metrics["last_authorization"],
        Value::Null,
        "the keenable passthrough must never send an Authorization header"
    );
    assert_eq!(metrics["mcp_initialize"], json!(1));
    assert_eq!(metrics["auth_failures"], json!(0));
    pool.close().await;

    // The notification completed the handshake without billing anything.
    let charged = harness.charged_ledger_rows().await;
    assert!(charged.is_empty(), "initialize/notifications bill nothing: {charged:?}");
}

/// Criterion: tools/list passthrough is FREE and returns EXACTLY the hosted
/// catalog (`search_web_pages` + `fetch_page_content`) verbatim.
#[tokio::test]
async fn keenable_tools_list_is_free_and_returns_exactly_both_tools() {
    let harness = spawn_keenable_harness("mcp-ke-toolslist", &["ke-pool-2"], &["ke-pool-2"]).await;
    let session = harness.initialize_session(1).await;

    let response = harness
        .keenable_mcp_call(
            Some(&session),
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let body = response.json();
    assert_eq!(body["jsonrpc"], json!("2.0"));
    assert_eq!(body["id"], json!(2));
    let tools: Vec<&str> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(
        tools,
        vec!["search_web_pages", "fetch_page_content"],
        "the hosted catalog passes through unchanged"
    );
    assert_eq!(
        body["result"]["tools"][0]["description"],
        json!("Search the live web with Keenable.")
    );

    let charged = harness.charged_ledger_rows().await;
    assert!(charged.is_empty(), "tools/list must not bill: {charged:?}");
    let logs = harness.token_log_rows().await;
    assert!(
        logs.iter()
            .any(|(kind, _, _)| kind.as_deref() == Some("mcp:tools/list")),
        "tools/list attempt row recorded with its kind: {logs:?}"
    );
    assert!(
        logs.iter().all(|(_, credits, _)| credits.unwrap_or(0) == 0),
        "no business credits on free methods: {logs:?}"
    );
    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["mcp_tools_list"], json!(1));
}

/// Criterion: tools/call search_web_pages bills exactly 1 credit under kind
/// `mcp:keenable/search_web_pages`, the upstream result passes through
/// verbatim, and the passive harvest persists the `_meta` balance.
#[tokio::test]
async fn keenable_tools_call_search_bills_one_credit_and_harvests_meta() {
    let harness = spawn_keenable_harness("mcp-ke-bill", &["ke-pool-3"], &["ke-pool-3"]).await;
    let session = harness.initialize_session(1).await;

    let response = harness
        .keenable_mcp_call(
            Some(&session),
            &KeenableMcpHarness::tools_call_message(
                2,
                "search_web_pages",
                json!({ "query": "rust mcp", "mode": "pro" }),
            ),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let body = response.json();
    assert_eq!(body["id"], json!(2));
    assert_eq!(body["result"]["structuredContent"]["query"], json!("rust mcp"));
    assert_eq!(body["result"]["structuredContent"]["mode"], json!("pro"));
    // The mock starts at 100_000 credits and bills 1 per successful call.
    assert_eq!(
        body["result"]["_meta"]["keenable/usage"]["credits_remaining"],
        json!(99_999)
    );

    let charged = harness.charged_ledger_rows().await;
    assert_eq!(charged.len(), 1, "exactly one ledger row: {charged:?}");
    assert_eq!(charged[0].0, "charged");
    assert_eq!(charged[0].1, 1, "1 credit for one successful search_web_pages");

    let pool = harness.pool().await;
    let (kind, credits): (Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT request_kind_key, business_credits FROM auth_token_logs \
         WHERE token_id = ? AND business_credits > 0 ORDER BY id DESC LIMIT 1",
    )
    .bind(&harness.token_id)
    .fetch_one(&pool)
    .await
    .expect("billed token log row");
    assert_eq!(kind.as_deref(), Some("mcp:keenable/search_web_pages"));
    assert_eq!(credits, Some(1));

    let log = sqlx::query(
        "SELECT request_kind_key, provider, upstream_operation FROM observability.request_logs \
         WHERE path = '/keenable/mcp' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("request log row");
    assert_eq!(
        log.get::<Option<String>, _>("request_kind_key").as_deref(),
        Some("mcp:keenable/search_web_pages")
    );
    assert_eq!(
        log.get::<Option<String>, _>("provider").as_deref(),
        Some("keenable")
    );
    assert_eq!(
        log.get::<Option<String>, _>("upstream_operation").as_deref(),
        Some("keenable_mcp")
    );

    let month_count: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT month_count FROM auth_token_quota WHERE token_id = ? LIMIT 1), 0)",
    )
    .bind(&harness.token_id)
    .fetch_one(&pool)
    .await
    .expect("token quota row");
    assert_eq!(month_count, 1);

    // Passive harvest: the _meta balance is persisted on the key with
    // quota_limit NULL and exactly one keenable_passive sample.
    let (quota_limit, quota_remaining, quota_renews_at): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as(
            "SELECT quota_limit, quota_remaining, quota_renews_at FROM api_keys \
             WHERE provider = 'keenable' AND deleted_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("keenable key row");
    assert_eq!(quota_remaining, Some(99_999));
    assert_eq!(quota_limit, None, "a prepaid balance has no plan limit");
    assert_eq!(quota_renews_at, None, "no calendar reset on a passive sample");
    let samples: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_key_quota_sync_samples s \
         JOIN api_keys k ON k.id = s.key_id \
         WHERE k.provider = 'keenable' AND s.source = 'keenable_passive'",
    )
    .fetch_one(&pool)
    .await
    .expect("passive sample scan");
    assert_eq!(samples, 1);
    pool.close().await;

    // Secret check: the pooled key rode X-API-Key, never Authorization.
    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["last_api_key"], json!("ke-pool-3"));
    assert_eq!(metrics["last_authorization"], Value::Null);
    assert_eq!(metrics["mcp_tools_call"], json!(1));
}

/// Plan todo 22: a hosted-MCP tools/call carrying the documented
/// X-RateLimit-* response headers leaves a persisted rate-limit window in the
/// existing meta KV while the credits_remaining harvest is unchanged.
#[tokio::test]
async fn keenable_mcp_tools_call_harvests_x_ratelimit_headers() {
    let harness = spawn_keenable_harness("mcp-ke-ratelimit", &["ke-pool-5"], &["ke-pool-5"]).await;
    let session = harness.initialize_session(1).await;

    let response = harness
        .keenable_mcp_call(
            Some(&session),
            &KeenableMcpHarness::tools_call_message(
                2,
                "search_web_pages",
                json!({ "query": "mcp ratelimit" }),
            ),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);

    // The passive X-RateLimit window is a SEPARATE record: the monthly
    // credits harvest still owns api_keys.quota_remaining and its sample.
    let snapshot = harness
        .keenable_rate_limit_meta("ke-pool-5")
        .await
        .expect("persisted MCP rate-limit snapshot");
    assert_eq!(snapshot["limit"], json!(10));
    assert_eq!(snapshot["remaining"], json!(9));
    assert_eq!(snapshot["reset"], json!("2026-12-31T23:59:59Z"));
    assert_eq!(snapshot["reset_at"], json!(1_798_761_599_i64));
    assert!(snapshot["captured_at"].as_i64().unwrap_or_default() > 0);

    let pool = harness.pool().await;
    let (quota_limit, quota_remaining, quota_renews_at): (Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as(
            "SELECT quota_limit, quota_remaining, quota_renews_at FROM api_keys \
             WHERE provider = 'keenable' AND deleted_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("keenable key row");
    assert_eq!(quota_remaining, Some(99_999));
    assert_eq!(quota_limit, None);
    assert_eq!(quota_renews_at, None);
    let samples: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_key_quota_sync_samples s \
         JOIN api_keys k ON k.id = s.key_id \
         WHERE k.provider = 'keenable' AND s.source = 'keenable_passive'",
    )
    .fetch_one(&pool)
    .await
    .expect("passive sample scan");
    assert_eq!(samples, 1);
    pool.close().await;
}

/// Criterion: tools/call fetch_page_content bills 1 credit under kind
/// `mcp:keenable/fetch_page_content` and the native fetch body passes through.
#[tokio::test]
async fn keenable_tools_call_fetch_bills_one_credit_with_fetch_kind() {
    let harness = spawn_keenable_harness("mcp-ke-fetch", &["ke-pool-4"], &["ke-pool-4"]).await;
    let session = harness.initialize_session(1).await;

    let response = harness
        .keenable_mcp_call(
            Some(&session),
            &KeenableMcpHarness::tools_call_message(
                2,
                "fetch_page_content",
                json!({ "url": "https://example.com/docs", "max_chars": 500 }),
            ),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    let body = response.json();
    assert_eq!(
        body["result"]["structuredContent"]["url"],
        json!("https://example.com/docs")
    );
    assert_eq!(
        body["result"]["structuredContent"]["title"],
        json!("Mock Page")
    );

    let charged = harness.charged_ledger_rows().await;
    assert_eq!(charged.len(), 1, "exactly one ledger row: {charged:?}");
    assert_eq!(charged[0].1, 1, "1 credit for one successful fetch_page_content");

    let pool = harness.pool().await;
    let (kind, credits): (Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT request_kind_key, business_credits FROM auth_token_logs \
         WHERE token_id = ? AND business_credits > 0 ORDER BY id DESC LIMIT 1",
    )
    .bind(&harness.token_id)
    .fetch_one(&pool)
    .await
    .expect("billed token log row");
    assert_eq!(kind.as_deref(), Some("mcp:keenable/fetch_page_content"));
    assert_eq!(credits, Some(1));
    pool.close().await;
}

/// Criterion (QA failure): a 401 on the PINNED key migrates the session onto
/// a second key — ONE strict acquire, ONE re-initialize, ONE rebind, ONE
/// replay — and the replayed call bills exactly once.
#[tokio::test]
async fn keenable_pinned_key_401_migrates_session_to_second_key() {
    let harness = spawn_keenable_harness(
        "mcp-ke-migrate",
        &["key-auth-after-init", "ke-good-2"],
        &["key-auth-after-init", "ke-good-2"],
    )
    .await;

    // Key-acquisition ties on never-used keys break on the random key-row
    // id, so the FIRST initialize can lease either pooled key. The ranking
    // is last_used_at ASC, so a SECOND initialize deterministically leases
    // the other (never-used) key — two rounds guarantee a session pinned to
    // the turncoat key (key-auth-after-init: authorize passes, tools/call
    // answers 401).
    let first = harness.initialize_session(1).await;
    let (session, extra_initialize) =
        if harness.session_pinned_key_secret(&first).await == "key-auth-after-init" {
            (first, 0u64)
        } else {
            let second = harness.initialize_session(2).await;
            assert_eq!(
                harness.session_pinned_key_secret(&second).await,
                "key-auth-after-init",
                "the second initialize must lease the never-used turncoat key"
            );
            (second, 1)
        };
    assert_eq!(
        harness.session_pinned_key_secret(&session).await,
        "key-auth-after-init"
    );

    // The session is pinned to the turncoat key; its tools/call answers 401.
    let response = harness
        .keenable_mcp_call(
            Some(&session),
            &KeenableMcpHarness::tools_call_message(
                2,
                "search_web_pages",
                json!({ "query": "migrate me" }),
            ),
        )
        .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "the migration ladder must replay the call on the second key: {}",
        response.body_text()
    );
    let body = response.json();
    assert_eq!(body["result"]["structuredContent"]["query"], json!("migrate me"));

    // The session row rebound to the second key.
    let row = harness.mcp_session_row(&session).await;
    let upstream_key_id = row
        .get::<Option<String>, _>("upstream_key_id")
        .expect("pinned upstream key id");
    let pool = harness.pool().await;
    let (key_secret, key_status): (String, String) =
        sqlx::query_as("SELECT api_key, status FROM api_keys WHERE id = ?")
            .bind(&upstream_key_id)
            .fetch_one(&pool)
            .await
            .expect("rebound key row");
    assert_eq!(key_secret, "ke-good-2", "session rebound to the second key");
    assert_eq!(key_status, "active");

    // The dead key was quarantined through the existing machinery.
    let quarantined: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_key_quarantines q \
         JOIN api_keys k ON k.id = q.key_id \
         WHERE k.api_key = 'key-auth-after-init' AND q.cleared_at IS NULL \
         AND q.reason_code = 'invalid_api_key'",
    )
    .fetch_one(&pool)
    .await
    .expect("quarantine scan");
    assert_eq!(quarantined, 1, "the 401 must quarantine the dead key");

    // Exactly ONE settlement: the replayed success bills, the 401 does not.
    let charged = harness.charged_ledger_rows().await;
    assert_eq!(charged.len(), 1, "one settlement for the replay: {charged:?}");
    assert_eq!(charged[0].1, 1);
    pool.close().await;

    // Mock lifecycle: the first initialize (+ the branch's second), the
    // rebind initialize (2 + extra_initialize), the rejected call + the
    // replayed call (2), one auth failure.
    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["mcp_initialize"], json!(2 + extra_initialize));
    assert_eq!(metrics["mcp_tools_call"], json!(2));
    assert_eq!(metrics["auth_failures"], json!(1));
    assert_eq!(metrics["last_api_key"], json!("ke-good-2"));
}

/// Criterion (QA failure): session resolution is PROVIDER-SCOPED — a proxy
/// session id minted on `/serpapi/mcp` returns 404 on `/keenable/mcp`.
#[tokio::test]
async fn keenable_surface_rejects_a_serpapi_minted_session_id() {
    let harness = spawn_keenable_harness_with_serpapi_pool(
        "mcp-ke-cross",
        &["ke-pool-5"],
        &["ke-pool-5", "key-good"],
        &["key-good"],
        false,
    )
    .await;

    // Mint a session on the SerpApi surface (the same stub serves it).
    let serpapi_init = harness
        .mcp_call("/serpapi/mcp", None, &KeenableMcpHarness::initialize_message(1))
        .await;
    assert_eq!(
        serpapi_init.status,
        StatusCode::OK,
        "serpapi initialize: {}",
        serpapi_init.body_text()
    );
    let serpapi_session = serpapi_init
        .session_header
        .clone()
        .expect("serpapi proxy session id");

    // That session id never resolves on the keenable surface.
    let crossed = harness
        .keenable_mcp_call(
            Some(&serpapi_session),
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    assert_eq!(
        crossed.status,
        StatusCode::NOT_FOUND,
        "a serpapi proxy session must not resolve on /keenable/mcp: {}",
        crossed.body_text()
    );
    let body = crossed.json();
    assert_eq!(body.get("error").and_then(Value::as_str), Some("session_unavailable"));

    // The keenable surface still initializes its own session afterwards.
    let keenable_session = harness.initialize_session(3).await;
    assert_ne!(keenable_session, serpapi_session);
    let row = harness.mcp_session_row(&keenable_session).await;
    assert_eq!(
        row.get::<Option<String>, _>("provider").as_deref(),
        Some("keenable")
    );
    let pool = harness.pool().await;
    let serpapi_provider: Option<Option<String>> = sqlx::query_scalar(
        "SELECT provider FROM mcp_sessions WHERE proxy_session_id = ?",
    )
    .bind(&serpapi_session)
    .fetch_one(&pool)
    .await
    .expect("serpapi session row");
    assert_eq!(
        serpapi_provider.unwrap().as_deref(),
        Some("serpapi"),
        "the serpapi session row keeps its own provider stamp"
    );
    pool.close().await;
}

/// Criterion (acceptance): no Tavily session is accepted on `/keenable/mcp`
/// — session resolution is provider-scoped, so a REAL Tavily control session
/// minted on `/mcp` (in-process Tavily-shaped mock, never a production
/// endpoint) returns 404 there and does not entangle the keenable surface.
#[tokio::test]
async fn tavily_control_session_is_rejected_on_keenable_mcp() {
    let harness = spawn_keenable_harness_with_serpapi_pool(
        "mcp-ke-tavily",
        &["ke-pool-9"],
        &["ke-pool-9"],
        &[],
        true,
    )
    .await;
    let tavily_counters = harness.tavily_counters.clone().expect("tavily mock");

    // Mint a REAL Tavily control session on the same token.
    let tavily_init = harness
        .mcp_call("/mcp", None, &KeenableMcpHarness::initialize_message(1))
        .await;
    assert_eq!(
        tavily_init.status,
        StatusCode::OK,
        "tavily initialize: {}",
        tavily_init.body_text()
    );
    let tavily_session = tavily_init
        .session_header
        .clone()
        .expect("tavily control session id");
    assert_eq!(
        tavily_counters
            .initialize
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the Tavily session is real (the initialize reached the mock upstream)"
    );

    // The Tavily control session never resolves on the keenable surface.
    let crossed = harness
        .keenable_mcp_call(
            Some(&tavily_session),
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
    assert_eq!(
        crossed.status,
        StatusCode::NOT_FOUND,
        "a Tavily control session must not resolve on /keenable/mcp: {}",
        crossed.body_text()
    );
    assert_eq!(
        crossed.json().get("error").and_then(Value::as_str),
        Some("session_unavailable")
    );

    // The keenable surface still initializes its own session afterwards.
    let keenable_session = harness.initialize_session(3).await;
    assert_ne!(keenable_session, tavily_session);
    let row = harness.mcp_session_row(&keenable_session).await;
    assert_eq!(
        row.get::<Option<String>, _>("provider").as_deref(),
        Some("keenable")
    );
    let pool = harness.pool().await;
    let tavily_provider: Option<String> = sqlx::query_scalar(
        "SELECT provider FROM mcp_sessions WHERE proxy_session_id = ?",
    )
    .bind(&tavily_session)
    .fetch_one(&pool)
    .await
    .expect("tavily session row");
    assert!(
        tavily_provider.is_none() || tavily_provider.as_deref() == Some("tavily"),
        "Tavily rows keep the byte-stable provider default: {tavily_provider:?}"
    );
    pool.close().await;
}

/// Criterion (SPEC): a GET opening an event stream is rejected with 405 and
/// never dials the upstream.
#[tokio::test]
async fn keenable_get_event_stream_is_rejected_405_without_upstream_call() {
    let harness = spawn_keenable_harness("mcp-ke-405", &["ke-pool-6"], &["ke-pool-6"]).await;

    let response = Client::new()
        .get(format!("{}/keenable/mcp", harness.base_url))
        .bearer_auth(&harness.token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("get event-stream request");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["mcp"], json!(0), "the 405 never reaches the upstream");
    let charged = harness.charged_ledger_rows().await;
    assert!(charged.is_empty());
}

/// Criterion (adversarial): a stale/unknown proxy session id answers 404
/// `session_unavailable` and never dials the upstream.
#[tokio::test]
async fn keenable_unknown_session_id_answers_404_without_upstream_call() {
    let harness = spawn_keenable_harness("mcp-ke-stale", &["ke-pool-7"], &["ke-pool-7"]).await;

    let response = harness
        .keenable_mcp_call(
            Some("no-such-session"),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    let body = response.json();
    assert_eq!(body.get("error").and_then(Value::as_str), Some("session_unavailable"));

    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["mcp"], json!(0));
    let charged = harness.charged_ledger_rows().await;
    assert!(charged.is_empty());
}

/// Criterion (adversarial): malformed JSON-RPC bodies pass through for the
/// upstream's own native error and bill nothing (missing-id attribution
/// defaults to error — never guess).
#[tokio::test]
async fn keenable_malformed_jsonrpc_bodies_pass_through_and_bill_nothing() {
    let harness = spawn_keenable_harness("mcp-ke-malformed", &["ke-pool-8"], &["ke-pool-8"]).await;
    let session = harness.initialize_session(1).await;

    // Non-JSON body: the upstream's native -32700 parse error passes through.
    let raw = Client::new()
        .post(format!("{}/keenable/mcp", harness.base_url))
        .bearer_auth(&harness.token)
        .header("content-type", "application/json")
        .header("mcp-session-id", &session)
        .body("this is not json-rpc")
        .send()
        .await
        .expect("malformed mcp request");
    assert_eq!(raw.status(), StatusCode::OK);
    let body: Value = raw.json().await.expect("native parse error body");
    assert_eq!(body["error"]["code"], json!(-32700));

    // A JSON-RPC method error also passes through without billing.
    let unknown = harness
        .keenable_mcp_call(
            Some(&session),
            &json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list" }),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::OK);
    assert_eq!(unknown.json()["error"]["code"], json!(-32601));

    let charged = harness.charged_ledger_rows().await;
    assert!(charged.is_empty(), "malformed/error bodies never bill: {charged:?}");
    let logs = harness.token_log_rows().await;
    assert!(
        logs.iter().all(|(_, credits, _)| credits.unwrap_or(0) == 0),
        "no business credits: {logs:?}"
    );
    let metrics = harness.mock_metrics().await;
    assert_eq!(metrics["mcp_tools_call"], json!(0), "no tools/call reached the stub");
}
