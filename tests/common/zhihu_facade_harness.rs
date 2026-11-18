//! Shared Zhihu REST facade harness (keenable-integration plan todo 9).
//!
//! Extracted from `provider_facades_contract.rs` to keep that file inside the
//! 3050-line Rust source budget (same pattern as the other `tests/common`
//! extractions). Spawns `mock_zhihu` + the real server binary with every other
//! provider upstream pinned to a dead local port, and exposes the sqlite
//! assertions the Zhihu facade cases share.

use super::*;

pub(crate) struct ZhihuHarness {
    pub(crate) base_url: String,
    pub(crate) mock_base_url: String,
    pub(crate) token: String,
    pub(crate) token_id: String,
    pub(crate) db_path: PathBuf,
    pub(crate) db_str: String,
    _server: ChildGuard,
    _mock: ChildGuard,
}

impl Drop for ZhihuHarness {
    fn drop(&mut self) {
        remove_sqlite_files(&self.db_path);
    }
}

/// Spawn mock_zhihu + the real server binary; every other provider
/// upstream points at a dead local port (stub-only rule by construction).
pub(crate) async fn spawn_harness(prefix: &str, pool_keys: &[&str]) -> ZhihuHarness {
    for attempt in 1..=5usize {
        if let Some(harness) = try_spawn_harness(prefix, pool_keys).await {
            return harness;
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    panic!("zhihu harness never became healthy after 5 spawn attempts");
}

async fn try_spawn_harness(prefix: &str, pool_keys: &[&str]) -> Option<ZhihuHarness> {
    let mock_port = reserve_local_port();
    let mock = Command::new(support_binaries::mock_zhihu_binary())
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
    .env("TAVILY_API_KEYS", "tvly-facade-contract-key")
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
    let mock_base_url = format!("http://127.0.0.1:{mock_port}");
    let (token, token_id) = create_test_token(&base_url).await;

    Some(ZhihuHarness {
        base_url,
        mock_base_url,
        token,
        token_id,
        db_path,
        db_str,
        _server: ChildGuard { child: server },
        _mock: ChildGuard { child: mock },
    })
}

impl ZhihuHarness {
    pub(crate) async fn pool(&self) -> sqlx::SqlitePool {
        connect_sqlite_test_pool(&self.db_str).await
    }

    pub(crate) async fn get_facade(&self, path_with_query: &str) -> (reqwest::StatusCode, Value) {
        self.get_facade_with_token(path_with_query, &self.token)
            .await
    }

    pub(crate) async fn get_facade_with_token(
        &self,
        path_with_query: &str,
        token: &str,
    ) -> (reqwest::StatusCode, Value) {
        let response = Client::new()
            .get(format!("{}{path_with_query}", self.base_url))
            .bearer_auth(token)
            .send()
            .await
            .expect("zhihu facade request");
        let status = response.status();
        let body = response.json().await.expect("json body");
        (status, body)
    }

    pub(crate) async fn mock_metrics(&self) -> Value {
        Client::new()
            .get(format!("{}/metrics", self.mock_base_url))
            .send()
            .await
            .expect("mock metrics request")
            .json::<Value>()
            .await
            .expect("mock metrics body")
    }

    pub(crate) async fn last_bodies(&self) -> Value {
        Client::new()
            .get(format!("{}/last-bodies", self.mock_base_url))
            .send()
            .await
            .expect("mock last-bodies request")
            .json::<Value>()
            .await
            .expect("mock last-bodies body")
    }
}

/// QA (keenable-integration todo 12): the REAL `mock_zhihu` tight plan
/// (`tok-quota-tight`: `zhihu_search` 3/10, `global_search` 5/20) drives the
/// admin manual sync-usage arm to persist `quota_remaining=3`,
/// `quota_limit=10` and a `NaturalDay` renewal anchor through the live
/// `/api/v1/quota` adapter.
#[tokio::test]
async fn real_mock_zhihu_tight_plan_persists_through_manual_sync() {
    let harness = spawn_harness("zhihu-quota-tight-real", &["tok-quota-tight"]).await;
    let pool = harness.pool().await;
    let key_id: String = sqlx::query_scalar(
        "SELECT id FROM api_keys WHERE provider = 'zhihu' AND deleted_at IS NULL \
         ORDER BY created_at ASC, rowid ASC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("zhihu key row");

    let response = Client::new()
        .post(format!("{}/api/keys/{key_id}/sync-usage", harness.base_url))
        .send()
        .await
        .expect("manual sync request");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["source"], json!("live"));
    assert_eq!(body["provider"], json!("zhihu"));
    assert_eq!(body["quota_limit"], json!(10));
    assert_eq!(body["quota_remaining"], json!(3));
    assert!(body["quota_renews_at"].as_i64().is_some());

    let (limit, remaining, renews_at): (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT quota_limit, quota_remaining, quota_renews_at FROM api_keys WHERE id = ?",
    )
    .bind(&key_id)
    .fetch_one(&pool)
    .await
    .expect("key quota row");
    assert_eq!((limit, remaining), (Some(10), Some(3)));
    assert!(renews_at.is_some(), "NaturalDay renewal anchor stored");

    let metrics = harness.mock_metrics().await;
    let quota_reads = metrics["quota"].as_u64().unwrap_or(0);
    assert!(
        quota_reads >= 1,
        "the live quota endpoint was read (scheduler startup may race one extra read): {quota_reads}"
    );
    assert_eq!(
        metrics["last_authorization"],
        json!("Bearer tok-quota-tight")
    );
    assert!(
        metrics["last_x_request_timestamp"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "server-generated request timestamp"
    );
}

pub(crate) async fn charged_credits(pool: &sqlx::SqlitePool, token_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(business_credits), 0) FROM billing_ledger \
              WHERE token_id = ? AND billing_state = 'charged'",
    )
    .bind(token_id)
    .fetch_one(pool)
    .await
    .expect("charged credit sum")
}

#[derive(sqlx::FromRow)]
pub(crate) struct ZhihuAttemptRow {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: Option<String>,
    pub(crate) status_code: i64,
    pub(crate) tavily_status_code: Option<i64>,
    pub(crate) result_status: String,
    pub(crate) request_kind_key: String,
    pub(crate) provider: String,
    pub(crate) upstream_operation: Option<String>,
    pub(crate) failure_kind: Option<String>,
    pub(crate) key_effect_code: String,
    pub(crate) response_body: Option<String>,
}

pub(crate) async fn attempt_row(pool: &sqlx::SqlitePool, token_id: &str) -> ZhihuAttemptRow {
    sqlx::query_as(
        "SELECT method, path, query, status_code, tavily_status_code, result_status, \
              request_kind_key, provider, upstream_operation, failure_kind, key_effect_code, \
              CAST(response_body AS TEXT) AS response_body \
              FROM observability.request_logs WHERE auth_token_id = ? \
              ORDER BY id DESC LIMIT 1",
    )
    .bind(token_id)
    .fetch_one(pool)
    .await
    .expect("zhihu attempt row")
}
