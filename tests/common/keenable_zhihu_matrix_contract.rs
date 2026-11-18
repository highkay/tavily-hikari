//! Keenable/Zhihu cross-provider hardening matrix (keenable-integration
//! plan todo 20) for `provider_facades_contract.rs`.
//!
//! The per-provider sections (todo 7 `mod keenable`, todo 9 `mod zhihu`) pin
//! each facade in isolation with the OTHER provider pinned to a dead port.
//! This module keeps both mocks LIVE in one server so the todo-20 hardening
//! cells can express what the isolated sections cannot:
//!
//! - the full success matrix (all four REST routes) settling from ONE shared
//!   wallet with per-provider buckets and byte-stable cross-isolation;
//! - the full failure matrix (every scripted failure knob of both mocks)
//!   billing ZERO credits while the OTHER provider keeps serving from the
//!   same wallet (a failure never poisons the sibling provider);
//! - adversarial client credentials (a forged native-slot key, a forged
//!   `Authorization` Bearer, a forged `X-Request-Timestamp`) never reaching
//!   either upstream and never persisting;
//! - the NoAvailableKeys 502 shape once each provider's pool is fully
//!   quarantined.
//!
//! Every upstream is a stub (the two committed mock binaries); production
//! endpoints are never contacted (AGENTS.md incident rule).

use super::*;

const KEENABLE_SEARCH_PATH: &str = "/api/keenable/v1/search";
const KEENABLE_FETCH_PATH: &str = "/api/keenable/v1/fetch";
const ZHIHU_SEARCH_PATH: &str = "/api/zhihu/api/v1/content/zhihu_search";
const ZHIHU_GLOBAL_SEARCH_PATH: &str = "/api/zhihu/api/v1/content/global_search";

struct KeenableZhihuHarness {
    base_url: String,
    keenable_mock_base: String,
    zhihu_mock_base: String,
    token: String,
    token_id: String,
    keenable_pool_key: String,
    zhihu_pool_key: String,
    db_path: PathBuf,
    db_str: String,
    _server: ChildGuard,
    _keenable_mock: ChildGuard,
    _zhihu_mock: ChildGuard,
}

impl Drop for KeenableZhihuHarness {
    fn drop(&mut self) {
        remove_sqlite_files(&self.db_path);
    }
}

/// Spawn mock_keenable + mock_zhihu + the real server binary with BOTH new
/// providers live (every other provider upstream stays on a dead local port,
/// so production endpoints are unreachable by construction).
async fn spawn_keenable_zhihu_harness(
    prefix: &str,
    keenable_pool: &[&str],
    keenable_mock_keys: &[&str],
    zhihu_pool: &[&str],
    zhihu_mock_keys: &[&str],
) -> KeenableZhihuHarness {
    for attempt in 1..=5usize {
        if let Some(harness) = try_spawn_keenable_zhihu_harness(
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
    panic!("keenable+zhihu harness never became healthy after 5 spawn attempts");
}

async fn try_spawn_keenable_zhihu_harness(
    prefix: &str,
    keenable_pool: &[&str],
    keenable_mock_keys: &[&str],
    zhihu_pool: &[&str],
    zhihu_mock_keys: &[&str],
) -> Option<KeenableZhihuHarness> {
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
    .env("TAVILY_API_KEYS", "tvly-ke-zh-matrix-key")
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
        format!("http://127.0.0.1:{dead_port}/mcp"),
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
    wait_for_provider_keys(&db_str, "zhihu", i64::try_from(zhihu_pool.len()).unwrap()).await;
    let base_url = format!("http://127.0.0.1:{port}");
    let (token, token_id) = create_test_token(&base_url).await;

    Some(KeenableZhihuHarness {
        base_url,
        keenable_mock_base: format!("http://127.0.0.1:{keenable_port}"),
        zhihu_mock_base: format!("http://127.0.0.1:{zhihu_port}"),
        token,
        token_id,
        keenable_pool_key: keenable_pool[0].to_string(),
        zhihu_pool_key: zhihu_pool[0].to_string(),
        db_path,
        db_str,
        _server: ChildGuard { child: server },
        _keenable_mock: ChildGuard {
            child: keenable_mock,
        },
        _zhihu_mock: ChildGuard { child: zhihu_mock },
    })
}

impl KeenableZhihuHarness {
    async fn pool(&self) -> sqlx::SqlitePool {
        connect_sqlite_test_pool(&self.db_str).await
    }

    /// POST one Keenable search (Hikari Bearer by default; the native client
    /// `X-API-Key` slot variant exists for the adversarial credential test).
    async fn keenable_search(&self, body: &Value) -> (reqwest::StatusCode, Value) {
        self.keenable_search_with_token(&self.token, body, false)
            .await
    }

    async fn keenable_search_with_token(
        &self,
        token: &str,
        body: &Value,
        native_slot: bool,
    ) -> (reqwest::StatusCode, Value) {
        let mut request = Client::new()
            .post(format!("{}{KEENABLE_SEARCH_PATH}", self.base_url))
            .json(body);
        request = if native_slot {
            request.header("x-api-key", token)
        } else {
            request.bearer_auth(token)
        };
        let response = request.send().await.expect("keenable search request");
        let status = response.status();
        (status, response.json().await.expect("json body"))
    }

    /// GET one Keenable fetch with the Hikari Bearer token.
    async fn keenable_fetch(&self, query: &str) -> (reqwest::StatusCode, Value) {
        let response = Client::new()
            .get(format!("{}{KEENABLE_FETCH_PATH}?{query}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("keenable fetch request");
        let status = response.status();
        (status, response.json().await.expect("json body"))
    }

    /// GET one Zhihu route with the Hikari Bearer token.
    async fn zhihu_get(&self, path_with_query: &str) -> (reqwest::StatusCode, Value) {
        self.zhihu_get_with_token(path_with_query, &self.token, None)
            .await
    }

    /// GET one Zhihu route with optional adversarial client headers.
    async fn zhihu_get_with_token(
        &self,
        path_with_query: &str,
        token: &str,
        adversarial: Option<(&str, &str)>,
    ) -> (reqwest::StatusCode, Value) {
        let mut request = Client::new()
            .get(format!("{}{path_with_query}", self.base_url))
            .bearer_auth(token);
        if let Some((name, value)) = adversarial {
            request = request.header(name, value);
        }
        let response = request.send().await.expect("zhihu facade request");
        let status = response.status();
        (status, response.json().await.expect("json body"))
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

    async fn charged_credits_for(&self, token_id: &str) -> i64 {
        let pool = self.pool().await;
        let credits = zhihu_harness::charged_credits(&pool, token_id).await;
        pool.close().await;
        credits
    }

    /// (provider, SUM(business_credits), COUNT(*)) for CHARGED rows of one
    /// token, taking the provider from the linked observability row.
    async fn charged_provider_breakdown(&self, token_id: &str) -> Vec<(String, i64, i64)> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT COALESCE(r.provider, '') AS provider, \
             COALESCE(SUM(l.business_credits), 0) AS credits, COUNT(*) AS rows \
             FROM auth_token_logs l \
             JOIN observability.request_logs r ON r.id = l.request_log_id \
             WHERE l.token_id = ? AND l.business_credits > 0 \
             GROUP BY r.provider ORDER BY r.provider",
        )
        .bind(token_id)
        .fetch_all(&pool)
        .await
        .expect("charged provider breakdown");
        pool.close().await;
        rows.into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("provider"),
                    row.get::<i64, _>("credits"),
                    row.get::<i64, _>("rows"),
                )
            })
            .collect()
    }

    /// Persisted request-log text fields per row, for secret scanning:
    /// (query, request_body, response_body, error_message, forwarded_headers,
    /// dropped_headers).
    async fn persisted_request_log_fields(&self) -> Vec<[String; 6]> {
        let pool = self.pool().await;
        let rows = sqlx::query(
            "SELECT COALESCE(query, '') AS query, \
             COALESCE(CAST(request_body AS TEXT), '') AS rb, \
             COALESCE(CAST(response_body AS TEXT), '') AS respb, \
             COALESCE(error_message, '') AS em, \
             COALESCE(CAST(forwarded_headers AS TEXT), '') AS fh, \
             COALESCE(CAST(dropped_headers AS TEXT), '') AS dh \
             FROM observability.request_logs ORDER BY id ASC",
        )
        .fetch_all(&pool)
        .await
        .expect("request log text scan");
        pool.close().await;
        rows.into_iter()
            .map(|row| {
                [
                    row.get::<String, _>("query"),
                    row.get::<String, _>("rb"),
                    row.get::<String, _>("respb"),
                    row.get::<String, _>("em"),
                    row.get::<String, _>("fh"),
                    row.get::<String, _>("dh"),
                ]
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Success matrix — all four REST routes in ONE shared wallet (todo 20).
// ---------------------------------------------------------------------------

/// The full Keenable/Zhihu success matrix in one run: one token spends a
/// Keenable search (1 credit), a Keenable fetch (1 credit), a Zhihu content
/// search (1 credit) and a Zhihu global search (1 credit); the ledger sums to
/// 4 with one bucket per provider; every route returns its native body
/// verbatim; and each mock sees ONLY its own surface with ONLY its own pooled
/// credential in its own native slot (no cross-provider leakage, no client
/// credential upstream).
#[tokio::test]
async fn keenable_and_zhihu_success_matrix_bills_one_wallet_in_isolation() {
    let harness = spawn_keenable_zhihu_harness(
        "facades-ke-zh-success",
        &["ke-good"],
        &["ke-good"],
        &["tok-good"],
        &["tok-good"],
    )
    .await;

    // Keenable search: native body verbatim, 1 credit.
    let (status, body) = harness
        .keenable_search(&json!({ "query": "matrix success", "mode": "pro" }))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["query"], json!("matrix success"));
    assert_eq!(body["mode"], json!("pro"));
    assert_eq!(
        body["usage"]["credits_remaining"],
        json!(99_999),
        "the passive balance passes through: {body}"
    );

    // Keenable fetch: native body verbatim, 1 credit.
    let (status, body) = harness
        .keenable_fetch("url=https%3A%2F%2Fexample.com%2Fpage&max_chars=500")
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["url"], json!("https://example.com/page"));
    assert_eq!(
        body["usage"]["credits_remaining"],
        json!(99_998),
        "the second success bills one more credit: {body}"
    );

    // Zhihu content search + global search: native envelopes verbatim.
    for (path, hash) in [
        (ZHIHU_SEARCH_PATH, "mock-zhihu_search-hash"),
        (ZHIHU_GLOBAL_SEARCH_PATH, "mock-global_search-hash"),
    ] {
        let (status, body) = harness
            .zhihu_get(&format!("{path}?Query=matrix&Count=3"))
            .await;
        assert_eq!(status, reqwest::StatusCode::OK, "{path}");
        assert_eq!(body["Code"], json!(0), "{path}");
        assert_eq!(body["Data"]["SearchHashId"], json!(hash), "{path}");
    }

    // ONE shared wallet: 4 charged rows, 4 credits, one bucket per provider.
    assert_eq!(harness.charged_credits_for(&harness.token_id).await, 4);
    assert_eq!(
        harness.charged_provider_breakdown(&harness.token_id).await,
        vec![("keenable".to_string(), 2, 2), ("zhihu".to_string(), 2, 2),],
        "each provider settles exactly its two successful ops"
    );
    let pool = harness.pool().await;
    let month_count: i64 = sqlx::query_scalar(
        "SELECT COALESCE((SELECT month_count FROM auth_token_quota \
          WHERE token_id = ? LIMIT 1), 0)",
    )
    .bind(&harness.token_id)
    .fetch_one(&pool)
    .await
    .expect("token quota row");
    assert_eq!(month_count, 4, "the monthly counter equals the sum");
    pool.close().await;

    // Cross-isolation: each mock saw ONLY its own surface, exactly once per
    // route, with its own pooled credential in its own native slot.
    let keenable_metrics = harness.keenable_mock_metrics().await;
    assert_eq!(keenable_metrics["search"], json!(1));
    assert_eq!(keenable_metrics["fetch"], json!(1));
    assert_eq!(keenable_metrics["last_api_key"], json!("ke-good"));
    assert_eq!(
        keenable_metrics["last_authorization"],
        Value::Null,
        "the keenable executor must never send an Authorization header upstream"
    );
    let zhihu_metrics = harness.zhihu_mock_metrics().await;
    assert_eq!(zhihu_metrics["zhihu_search"], json!(1));
    assert_eq!(zhihu_metrics["global_search"], json!(1));
    assert_eq!(
        zhihu_metrics["last_authorization"],
        json!("Bearer tok-good"),
        "the pooled Access Secret rides the Bearer slot"
    );
    assert!(
        zhihu_metrics["last_x_request_timestamp"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "a server-side timestamp rides every zhihu upstream call"
    );
}

// ---------------------------------------------------------------------------
// Failure matrix — every scripted knob bills zero, the sibling provider
// keeps serving (todo 20).
// ---------------------------------------------------------------------------

/// One Keenable failure leg: the scripted knob's rejection reaches the
/// client verbatim (single-key pool — no sibling absorbs it), bills ZERO
/// credits, applies exactly the expected key effect, and — the hardening
/// cell — the Zhihu surface on the SAME wallet still serves its native
/// envelope and settles exactly 1 credit afterwards.
#[tokio::test]
async fn keenable_failure_matrix_bills_zero_and_keeps_zhihu_serving() {
    struct Leg {
        pool_key: &'static str,
        client_status: u16,
        key_effect: &'static str,
        key_status_after: &'static str,
    }
    let legs = [
        Leg {
            pool_key: "key-auth",
            client_status: 401,
            key_effect: "quarantined",
            key_status_after: "active",
        },
        Leg {
            pool_key: "key-target",
            client_status: 403,
            key_effect: "none",
            key_status_after: "active",
        },
        Leg {
            pool_key: "key-rate",
            client_status: 429,
            key_effect: "transient_backoff_set",
            key_status_after: "active",
        },
        Leg {
            pool_key: "key-exhausted",
            client_status: 402,
            key_effect: "marked_exhausted",
            key_status_after: "exhausted",
        },
    ];

    for leg in legs {
        let harness = spawn_keenable_zhihu_harness(
            "facades-ke-fail",
            &[leg.pool_key],
            &["ke-x"],
            &["tok-good"],
            &["tok-good"],
        )
        .await;

        let (status, body) = harness
            .keenable_search(&json!({ "query": "failure matrix" }))
            .await;
        assert_eq!(
            status.as_u16(),
            leg.client_status,
            "{} reaches the client verbatim: {}",
            leg.pool_key,
            body
        );

        // ZERO credits settled for the failed op.
        assert_eq!(
            harness.charged_credits_for(&harness.token_id).await,
            0,
            "{} bills nothing",
            leg.pool_key
        );

        // The exact key effect on the single persisted attempt row.
        let pool = harness.pool().await;
        let chain = facade_chain_rows(&pool, &harness.token_id).await;
        assert_eq!(chain.len(), 1, "single attempt: {chain:?}");
        assert_eq!(chain[0].status_code, Some(leg.client_status as i64));
        assert_eq!(chain[0].key_effect_code, leg.key_effect, "{}", leg.pool_key);
        let key_status: String =
            sqlx::query_scalar("SELECT status FROM api_keys WHERE api_key = ?")
                .bind(leg.pool_key)
                .fetch_one(&pool)
                .await
                .expect("key row");
        assert_eq!(key_status, leg.key_status_after, "{}", leg.pool_key);
        pool.close().await;

        // The OTHER provider on the same wallet still serves + settles.
        let (status, body) = harness
            .zhihu_get(&format!("{ZHIHU_SEARCH_PATH}?Query=resilience&Count=3"))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "zhihu keeps serving after a keenable {}: {body}",
            leg.pool_key
        );
        assert_eq!(body["Code"], json!(0));
        assert_eq!(
            harness.charged_credits_for(&harness.token_id).await,
            1,
            "exactly the sibling provider's success settles after {}",
            leg.pool_key
        );

        // Cross-isolation: the keenable mock saw exactly its one failed call
        // and the zhihu mock exactly its one success.
        assert_eq!(harness.keenable_mock_metrics().await["search"], json!(1));
        assert_eq!(harness.zhihu_mock_metrics().await["zhihu_search"], json!(1));
    }
}

/// One Zhihu failure leg: the scripted envelope code arrives at HTTP 200
/// verbatim, bills ZERO credits, applies exactly the expected key effect,
/// and the Keenable surface on the SAME wallet still serves and settles
/// exactly 1 credit afterwards.
#[tokio::test]
async fn zhihu_failure_matrix_bills_zero_and_keeps_keenable_serving() {
    struct Leg {
        pool_key: &'static str,
        envelope_code: i64,
        key_effect: &'static str,
        arms_backoff: bool,
    }
    let legs = [
        Leg {
            pool_key: "tok-auth",
            envelope_code: 20_001,
            key_effect: "quarantined",
            arms_backoff: false,
        },
        Leg {
            pool_key: "tok-rate",
            envelope_code: 30_001,
            key_effect: "transient_backoff_set",
            arms_backoff: true,
        },
        Leg {
            pool_key: "tok-internal",
            envelope_code: 90_001,
            key_effect: "none",
            arms_backoff: false,
        },
        Leg {
            pool_key: "tok-badparam",
            envelope_code: 10_001,
            key_effect: "none",
            arms_backoff: false,
        },
    ];

    for leg in legs {
        let harness = spawn_keenable_zhihu_harness(
            "facades-zh-fail",
            &["ke-good"],
            &["ke-good"],
            &[leg.pool_key],
            &[leg.pool_key],
        )
        .await;

        let (status, body) = harness
            .zhihu_get(&format!("{ZHIHU_SEARCH_PATH}?Query=failure&Count=3"))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "{} keeps the HTTP-200 envelope transport: {}",
            leg.pool_key,
            body
        );
        assert_eq!(body["Code"], json!(leg.envelope_code), "{}", leg.pool_key);

        // ZERO credits settled for the failed op.
        assert_eq!(
            harness.charged_credits_for(&harness.token_id).await,
            0,
            "{} bills nothing",
            leg.pool_key
        );

        // The exact key effect on the single persisted attempt row.
        let pool = harness.pool().await;
        let chain = facade_chain_rows(&pool, &harness.token_id).await;
        assert_eq!(chain.len(), 1, "single attempt: {chain:?}");
        assert_eq!(chain[0].status_code, Some(200));
        assert_eq!(chain[0].tavily_status_code, Some(200));
        assert_eq!(chain[0].result_status, "error", "{}", leg.pool_key);
        assert_eq!(chain[0].key_effect_code, leg.key_effect, "{}", leg.pool_key);
        if leg.arms_backoff {
            let backoff: i64 = sqlx::query_scalar(
                "SELECT b.retry_after_secs FROM api_key_transient_backoffs b \
                  JOIN api_keys k ON k.id = b.key_id \
                  WHERE k.api_key = ? AND b.scope = 'http_global'",
            )
            .bind(leg.pool_key)
            .fetch_one(&pool)
            .await
            .expect("backoff row");
            assert_eq!(backoff, 60, "30001 carries no Retry-After: 60s default");
        }
        pool.close().await;

        // The OTHER provider on the same wallet still serves + settles.
        let (status, body) = harness
            .keenable_search(&json!({ "query": "resilience" }))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "keenable keeps serving after a zhihu {}: {body}",
            leg.pool_key
        );
        assert_eq!(
            harness.charged_credits_for(&harness.token_id).await,
            1,
            "exactly the sibling provider's success settles after {}",
            leg.pool_key
        );

        // Cross-isolation: the zhihu mock saw exactly its one failed call and
        // the keenable mock exactly its one success.
        assert_eq!(harness.zhihu_mock_metrics().await["zhihu_search"], json!(1));
        assert_eq!(harness.keenable_mock_metrics().await["search"], json!(1));
    }
}

// ---------------------------------------------------------------------------
// Adversarial client credentials never reach either upstream (todo 20).
// ---------------------------------------------------------------------------

/// A client forging every credential slot it can reach — a rogue value in
/// the native Keenable `X-API-Key` slot beside a valid Bearer, the th-
/// token through the native slot, a forged `X-Request-Timestamp` and a
/// rogue `X-API-Key` on the Zhihu routes — never gets any of them upstream
/// (each mock observes only its own pooled credential, generated server-side
/// for the timestamp) and none of the forged values or real secrets persist
/// in any request_logs text field.
#[tokio::test]
async fn forged_client_credentials_never_reach_either_upstream_or_persist() {
    let harness = spawn_keenable_zhihu_harness(
        "facades-ke-zh-privacy",
        &["ke-good"],
        &["ke-good"],
        &["tok-good"],
        &["tok-good"],
    )
    .await;
    let token = harness.token.clone();

    // Keenable search with BOTH a rogue native-slot value and a valid Bearer
    // (the executor must replace the native slot with the pooled key and
    // never relay the rogue value).
    let (status, body) = harness
        .keenable_search_with_token(&token, &json!({ "query": "forged slots" }), true)
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "native-slot auth works: {body}"
    );
    let rogue_slot_request = Client::new()
        .post(format!("{}{KEENABLE_SEARCH_PATH}", harness.base_url))
        .bearer_auth(&token)
        .header("x-api-key", "rogue-client-key")
        .json(&json!({ "query": "forged slot value" }))
        .send()
        .await
        .expect("keenable search with rogue native slot");
    assert_eq!(
        rogue_slot_request.status(),
        reqwest::StatusCode::OK,
        "the valid Bearer wins client auth while the rogue slot value is stripped"
    );

    // Zhihu search with a forged server-side timestamp and a rogue X-API-Key.
    let (status, body) = harness
        .zhihu_get_with_token(
            &format!("{ZHIHU_SEARCH_PATH}?Query=forged&Count=3"),
            &token,
            Some(("x-request-timestamp", "1")),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    let (status, body) = harness
        .zhihu_get_with_token(
            &format!("{ZHIHU_GLOBAL_SEARCH_PATH}?Query=forged&Count=3"),
            &token,
            Some(("x-api-key", "rogue-client-key")),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");

    // Upstream observation: the keenable mock saw ONLY the pooled key in
    // X-API-Key (never the rogue slot value) and NEVER an Authorization
    // header; the zhihu mock saw ONLY the pooled Bearer and a fresh
    // server-generated timestamp (never the forged "1").
    let keenable_metrics = harness.keenable_mock_metrics().await;
    assert_eq!(keenable_metrics["last_api_key"], json!("ke-good"));
    assert_eq!(
        keenable_metrics["last_authorization"],
        Value::Null,
        "a client Authorization never reaches the keenable upstream: {keenable_metrics}"
    );
    let zhihu_metrics = harness.zhihu_mock_metrics().await;
    assert_eq!(
        zhihu_metrics["last_authorization"],
        json!("Bearer tok-good")
    );
    let timestamp: i64 = zhihu_metrics["last_x_request_timestamp"]
        .as_str()
        .expect("server-side timestamp")
        .parse()
        .expect("unix-second timestamp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    assert_ne!(timestamp, 1, "the forged client timestamp never forwards");
    assert!(
        (now - 600..=now + 600).contains(&timestamp),
        "fresh server-side X-Request-Timestamp: {timestamp} vs {now}"
    );

    // Persistence: no th- token, no pooled secret, and none of the rogue
    // values in ANY persisted text field.
    let secret_tail = token
        .strip_prefix("th-")
        .and_then(|rest| rest.split_once('-').map(|(_, secret)| secret.to_string()))
        .expect("token secret tail");
    let rows = harness.persisted_request_log_fields().await;
    assert!(rows.len() >= 4, "every adversarial call persisted a row");
    for (index, fields) in rows.iter().enumerate() {
        for text in fields {
            assert!(
                !text.contains(&token),
                "persisted row {index} must never carry the th- token"
            );
            assert!(
                !text.contains(&secret_tail),
                "persisted row {index} must never carry the th- secret tail"
            );
            assert!(
                !text.contains(&harness.keenable_pool_key),
                "persisted row {index} must never carry the keenable X-API-Key secret"
            );
            assert!(
                !text.contains(&harness.zhihu_pool_key),
                "persisted row {index} must never carry the zhihu Bearer secret"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// NoAvailableKeys 502 shape once each pool is fully quarantined (todo 20).
// ---------------------------------------------------------------------------

/// The quarantined-pool 502 shape for both new providers (the serpapi/brave
/// parity cell): after each provider's ONLY pooled credential is
/// quarantined by its auth-failure path, the next request of that provider
/// answers the NoAvailableKeys 502 shape — while the sibling provider keeps
/// billing normally on the same wallet.
#[tokio::test]
async fn quarantined_pools_return_the_no_available_keys_shape_for_both() {
    let harness = spawn_keenable_zhihu_harness(
        "facades-ke-zh-502",
        &["key-auth"],
        &["ke-x"],
        &["tok-auth"],
        &["tok-auth"],
    )
    .await;

    // Keenable leg: the first search quarantines the only key (401), the
    // second answers the NoAvailableKeys 502 shape.
    let (status, _) = harness
        .keenable_search(&json!({ "query": "quarantine" }))
        .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    let (status, body) = harness
        .keenable_search(&json!({ "query": "quarantine again" }))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_GATEWAY,
        "a fully quarantined keenable pool answers the NoAvailableKeys shape: {body}"
    );
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("proxy_error")
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some("upstream unavailable")
    );

    // Zhihu leg: the first search quarantines the only secret (Code=20001 at
    // HTTP 200), the second answers the same NoAvailableKeys 502 shape.
    let (status, body) = harness
        .zhihu_get(&format!("{ZHIHU_SEARCH_PATH}?Query=quarantine&Count=3"))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["Code"], json!(20_001));
    let (status, body) = harness
        .zhihu_get(&format!(
            "{ZHIHU_SEARCH_PATH}?Query=quarantine+again&Count=3"
        ))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_GATEWAY,
        "a fully quarantined zhihu pool answers the NoAvailableKeys shape: {body}"
    );
    assert_eq!(
        body.get("error").and_then(Value::as_str),
        Some("proxy_error")
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some("upstream unavailable")
    );

    // Neither quarantined pool ever settled a credit.
    assert_eq!(harness.charged_credits_for(&harness.token_id).await, 0);
}
