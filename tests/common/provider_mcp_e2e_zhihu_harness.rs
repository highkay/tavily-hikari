// ---------------------------------------------------------------------------
// Zhihu MCP harness (real server binary + real mock_zhihu binary).
// keenable-integration plan todo 10: `/zhihu/mcp` local-facade coverage.
// ---------------------------------------------------------------------------

struct ZhihuMcpHarness {
    base_url: String,
    mock_base: String,
    token: String,
    token_id: String,
    db_path: PathBuf,
    db_str: String,
    _server: ChildGuard,
    _mock: ChildGuard,
}

impl Drop for ZhihuMcpHarness {
    fn drop(&mut self) {
        remove_sqlite_files(&self.db_path);
    }
}

/// Spawn mock_zhihu + the real server binary. Every NON-zhihu provider upstream
/// (Tavily included) points at a dead local port, so production endpoints are
/// unreachable by construction (stub-only rule).
async fn spawn_zhihu_mcp_harness(prefix: &str, pool_keys: &[&str]) -> ZhihuMcpHarness {
    for attempt in 1..=5usize {
        if let Some(harness) = try_spawn_zhihu_mcp_harness(prefix, pool_keys).await {
            return harness;
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    panic!("zhihu mcp harness never became healthy after 5 spawn attempts");
}

async fn try_spawn_zhihu_mcp_harness(prefix: &str, pool_keys: &[&str]) -> Option<ZhihuMcpHarness> {
    let mock_port = reserve_local_port();
    let mock = Command::new(support_binaries::resolve_support_binary(
        "MOCK_ZHIHU_TEST_BIN",
        env!("CARGO_BIN_EXE_mock_zhihu"),
    ))
    .arg("--bind")
    .arg(format!("127.0.0.1:{mock_port}"))
    .arg("--tokens")
    .arg(pool_keys.join(","))
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
    .env("TAVILY_API_KEYS", "tvly-mcp-e2e-zhihu-key")
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
    .env("KEENABLE_UPSTREAM", format!("http://127.0.0.1:{dead_port}"))
    .env("ZHIHU_UPSTREAM", format!("http://127.0.0.1:{mock_port}"))
    .env("ZHIHU_API_KEYS", pool_keys.join(","))
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
        drop(ChildGuard { child: mock });
        remove_sqlite_files(&db_path);
        return None;
    }
    wait_for_provider_keys(&db_str, "zhihu", i64::try_from(pool_keys.len()).unwrap()).await;
    let base_url = format!("http://127.0.0.1:{port}");
    let (token, token_id) = create_test_token(&base_url).await;

    Some(ZhihuMcpHarness {
        base_url,
        mock_base: format!("http://127.0.0.1:{mock_port}"),
        token,
        token_id,
        db_path,
        db_str,
        _server: ChildGuard { child: server },
        _mock: ChildGuard { child: mock },
    })
}

impl ZhihuMcpHarness {
    async fn pool(&self) -> sqlx::SqlitePool {
        connect_sqlite_test_pool(&self.db_str).await
    }

    /// POST one JSON-RPC message to `/zhihu/mcp` with the Hikari Bearer token
    /// and (optionally) an `mcp-session-id` header.
    async fn zhihu_mcp_call(
        &self,
        session_id: Option<&str>,
        message: &Value,
    ) -> McpCallResult {
        let mut request = Client::new()
            .post(format!("{}/zhihu/mcp", self.base_url))
            .bearer_auth(&self.token)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id);
        }
        let response = request.json(message).send().await.expect("zhihu mcp request");
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

    /// POST + decode the SSE-wrapped JSON-RPC payload, asserting the 200 the
    /// happy tools/call and control-plane paths carry.
    async fn zhihu_mcp_json(&self, message: &Value) -> Value {
        let result = self.zhihu_mcp_call(None, message).await;
        assert_eq!(
            result.status,
            StatusCode::OK,
            "local facade answers tools/call + methods at HTTP 200: {}",
            result.body_text()
        );
        parse_sse_json(&result.body)
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

    fn tools_call(id: u64, tool: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        })
    }

    /// POST initialize (session-less local response — no mcp-session-id).
    async fn initialize(&self, id: u64) -> Value {
        let result = self.zhihu_mcp_call(None, &Self::initialize_message(id)).await;
        assert_eq!(result.status, StatusCode::OK, "initialize 200");
        assert!(
            result.session_header.is_none(),
            "session-less local facade must not mint an mcp-session-id: {:?}",
            result.session_header
        );
        parse_sse_json(&result.body)
    }

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

    async fn mock_last_bodies(&self) -> Value {
        Client::new()
            .get(format!("{}/last-bodies", self.mock_base))
            .send()
            .await
            .expect("mock last-bodies request")
            .json::<Value>()
            .await
            .expect("mock last-bodies json")
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

    /// (provider, request_kind_key, result_status) of the latest
    /// `/zhihu/mcp` request_logs row (the provider-stamped trail).
    async fn latest_request_log_row(&self) -> (Option<String>, Option<String>, String) {
        let pool = self.pool().await;
        let row = sqlx::query(
            "SELECT provider, request_kind_key, result_status FROM observability.request_logs \
             WHERE path = '/zhihu/mcp' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("zhihu request log row");
        let out = (
            row.get::<Option<String>, _>("provider"),
            row.get::<Option<String>, _>("request_kind_key"),
            row.get::<String, _>("result_status"),
        );
        pool.close().await;
        out
    }

    /// True when any persisted `/zhihu/mcp` request_logs row carries the
    /// pooled Access Secret in a body column (the redaction gate: must be
    /// false in every scenario).
    async fn request_log_bodies_leak_secret(&self, pool_key: &str) -> bool {
        let pool = self.pool().await;
        let pattern = format!("%{pool_key}%");
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM observability.request_logs WHERE path = '/zhihu/mcp' \
             AND (CAST(request_body AS TEXT) LIKE ? OR CAST(response_body AS TEXT) LIKE ?)",
        )
        .bind(&pattern)
        .bind(&pattern)
        .fetch_one(&pool)
        .await
        .expect("leak scan");
        pool.close().await;
        leaked > 0
    }

    /// Count of ACTIVE quarantine rows for the pooled zhihu key (the todo-9
    /// record path's `Code=20001` quarantine check).
    async fn active_quarantine_count(&self, pool_key: &str) -> i64 {
        let pool = self.pool().await;
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM api_key_quarantines q JOIN api_keys k ON k.id = q.key_id \
             WHERE k.api_key = ? AND q.cleared_at IS NULL \
             AND q.reason_code = 'invalid_api_key'",
        )
        .bind(pool_key)
        .fetch_one(&pool)
        .await
        .expect("quarantine scan");
        pool.close().await;
        count
    }

    /// Seed an ACTIVE Tavily control session row (provider NULL = the Tavily
    /// default tag) bound to this token, so a REAL active Tavily session id can
    /// be presented on the session-less zhihu facade.
    async fn seed_tavily_control_session(&self, proxy_session_id: &str) {
        let pool = self.pool().await;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO mcp_sessions \
             (proxy_session_id, auth_token_id, gateway_mode, experiment_variant, provider, \
              created_at, updated_at, expires_at) \
             VALUES (?, ?, 'upstream_mcp', 'control', NULL, ?, ?, ?)",
        )
        .bind(proxy_session_id)
        .bind(&self.token_id)
        .bind(now)
        .bind(now)
        .bind(now + 3600)
        .execute(&pool)
        .await
        .expect("seed tavily control session row");
        pool.close().await;
    }
}
