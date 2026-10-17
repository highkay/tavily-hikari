// Retry budget, deadline and concurrency gates (plan provider-key-429-retry todo 8).
    //
    // `include!`d from `tavily_http_search.rs` for the same reason as the sibling
    // `key_retry.rs` fragment: the parent file sits at its `rust_source_line_budgets`
    // ceiling (3219/3250) and the plan forbids raising a budget. The include keeps the
    // module path - and with it the CI manifest prefix
    // `server::tests::tavily_http_search::tavily_` - so no manifest edit is needed
    // either. rustfmt never visits an include!-only file, so the four-space module
    // indentation below is kept by hand.
    //
    // Division of labour with the sibling fragment (todo 6/7), which already pins the
    // rotation SHAPE: 429 RA=0 / 432 / upstream 5xx / transport failure rotate to a
    // second key, a request-shaped 4xx plus `/research` and `/crawl` stay on a single
    // attempt, an empty pool passes the first hit through byte-for-byte and RA=45
    // refuses the chain with its header preserved. This battery pins the BUDGET that
    // shape runs inside:
    // * zero sleep - RA=0, RA absent, a bare 401 (which arms a >=120s auth-block
    //   cooldown) and a 432 (which flips the key to exhausted) all finish their
    //   rotation inside a generous bound, and the cases whose rejection arms a
    //   cooldown additionally prove the cooldown was still pending when the chain had
    //   already returned, which no wait-based implementation can fake;
    // * the 5s deadline is a gate, not a comment - a first hit that only answers after
    //   the deadline is served byte-for-byte and never triggers a second upstream call,
    //   with an eligible second key sitting untouched in the pool;
    // * the 2-attempt ceiling is the budget, not pool exhaustion - a three-key pool
    //   whose every attempt rejects still makes exactly two upstream calls and leaves
    //   the third key pristine;
    // * two concurrent logical requests rotate independently - each keeps its own
    //   two-row chain, its own settlement and its own response bytes, with both first
    //   attempts rendezvoused on a barrier so "concurrent" is proven, not hoped.
    //
    // Delays exist ONLY inside the mocks below; production stays zero-sleep.
    //
    // The budget's env override point is deliberately NOT driven from here:
    // `KeyRetryBudget::from_env` reads the process environment without taking
    // `lock_process_env`, and cargo runs this suite's tests in parallel threads, so
    // setting `KEY_RETRY_MAX_ATTEMPTS` / `KEY_RETRY_DEADLINE_SECS` would leak into
    // every concurrently running retry test. The override parser is pinned where it is
    // pure instead - `proxy_key_retry`'s
    // `cross_key_retry_budget_env_parsing_clamps_and_falls_back`.

    /// Generous wall-clock bound for a chain that must not sleep. Every mock below
    /// answers in microseconds, so a healthy rotation completes far inside a second;
    /// 15s stays an order of magnitude above that while remaining 2x below the
    /// SHORTEST wait a sleep-based implementation would have to sit out (an RA=0
    /// rejection is clamped up to a 30s cooldown, a Retry-After-less 429 to 60s, a
    /// bare 401 to >=120s). Waiting is therefore impossible to hide, and a merely slow
    /// host still passes.
    const RETRY_BUDGET_IMMEDIATE_BOUND: Duration = Duration::from_secs(15);

    /// The kernel's default deadline (`KEY_RETRY_DEADLINE_SECS_DEFAULT`), which the
    /// executor resolves through `KeyRetryBudget::from_env`. A slow first hit must
    /// answer LATER than this for the deadline gate to fire.
    const RETRY_BUDGET_DEADLINE: Duration = Duration::from_secs(5);

    /// How long the deadline case's mock holds the first hit before answering 429.
    /// Only just past the deadline: `tokio::time::sleep` never returns early, so the
    /// gate fires deterministically, while the suite's wall clock stays sane.
    const RETRY_BUDGET_SLOW_FIRST_HIT: Duration = Duration::from_secs(6);

    /// Bare 401 body: it carries no quarantine keyword (`deactivated` / `revoked` /
    /// `invalid api key`), so it classifies into the auth-block family - long cooldown,
    /// key still `active` - instead of quarantining the key. Same literal the
    /// classifier pin uses (`src/tests/provider_failure_classification.rs`).
    const RETRY_BUDGET_AUTH_BLOCKED_BODY: &str = r#"{"error":"Unauthorized"}"#;

    /// `api_key_transient_backoffs.reason_code` of a rate-limited attempt.
    const RETRY_BUDGET_RATE_LIMIT_REASON: &str = "upstream_rate_limited_429";
    /// `api_key_transient_backoffs.reason_code` of a bare 401/403 (plan todo 1).
    const RETRY_BUDGET_AUTH_BLOCK_REASON: &str = "upstream_auth_blocked";

    /// The two logical requests of the concurrency case, identified by their query.
    const RETRY_BUDGET_CONCURRENT_QUERY_A: &str = "retry-budget-concurrent-a";
    const RETRY_BUDGET_CONCURRENT_QUERY_B: &str = "retry-budget-concurrent-b";

    /// Mock for the deadline gate: the FIRST attempt on `/search` is held for
    /// `first_hit_delay` before it answers 429 with `Retry-After: 0`, and every later
    /// attempt would answer immediately with the second-hit success. Only the mock
    /// sleeps - the executor under test stays zero-sleep, which is exactly what the
    /// case proves: by the time the buffered 429 exists, the caller's own deadline has
    /// already been spent, so no second upstream call may follow even though the pool
    /// holds a willing key and the mock would have served it.
    async fn spawn_http_retry_budget_slow_first_hit_mock(
        first_hit_delay: Duration,
    ) -> (SocketAddr, Arc<Mutex<Vec<CrossKeyRetryAttempt>>>) {
        let attempts = Arc::new(Mutex::new(Vec::<CrossKeyRetryAttempt>::new()));
        let per_path_attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let plan = CrossKeyRetryMockPlan::rotating_after_rejection(
            CrossKeyRetryRejectedRoute::JsonPost("/search"),
            429,
            Some("0"),
            CROSS_KEY_RETRY_RATE_LIMITED_BODY,
        );

        let search_route = {
            let attempts = attempts.clone();
            let per_path_attempts = per_path_attempts.clone();
            move |uri: Uri, headers: HeaderMap, Json(body): Json<Value>| {
                let attempts = attempts.clone();
                let per_path_attempts = per_path_attempts.clone();
                async move {
                    let api_key = body
                        .get("api_key")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    assert_eq!(
                        api_key,
                        cross_key_retry_upstream_bearer(&headers),
                        "body api_key and upstream bearer must carry the same key on /search"
                    );
                    assert!(
                        !api_key.is_empty(),
                        "upstream /search must receive an injected Tavily key"
                    );
                    let path = uri.path().to_string();
                    let attempt = cross_key_retry_record_attempt(
                        &attempts,
                        &per_path_attempts,
                        &path,
                        &api_key,
                    );
                    // The delay belongs to the first hit only: a chain that wrongly
                    // retried would fail fast on the attempt count instead of also
                    // paying the delay twice.
                    if attempt == 1 {
                        tokio::time::sleep(first_hit_delay).await;
                    }
                    plan.response_for(&path, attempt)
                }
            }
        };

        // A usage probe is not a business attempt: it answers but is never recorded,
        // so the recorder stays a faithful upstream-call count.
        let usage_route = || async {
            cross_key_retry_json_response(StatusCode::OK, None, CROSS_KEY_RETRY_USAGE_BODY)
        };

        let app = Router::new()
            .route("/search", post(search_route))
            .route("/usage", get(usage_route));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (addr, attempts)
    }

    /// One upstream attempt of the concurrency case, recorded with the client query
    /// that identifies which logical request it belongs to.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RetryBudgetConcurrencyAttempt {
        api_key: String,
        query: String,
    }

    /// Success body that echoes the caller's query, so a served response can only have
    /// come from its own logical chain - the byte-level half of the no-cross-talk pin.
    fn retry_budget_echo_success_body(query: &str) -> Vec<u8> {
        let body = serde_json::json!({
            "status": 200,
            "results": [{ "title": format!("retry-budget-echo:{query}") }],
            "usage": { "credits": 1 },
        });
        serde_json::to_vec(&body).expect("encode echo success body")
    }

    /// JSON response over owned bytes (the sibling helper only takes static bodies).
    fn retry_budget_json_response(status: StatusCode, body: Vec<u8>) -> Response {
        Response::builder()
            .status(status)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("build retry budget mock response")
    }

    /// Mock for the concurrency gate: each logical request - identified by its own
    /// `query` - is rejected with 429 `Retry-After: 0` on its FIRST attempt and served
    /// a query-echoing success on every later attempt, whatever key arrives. Both first
    /// attempts rendezvous on `rendezvous` before either is answered, so the two chains
    /// are provably in flight at the same instant; `rendezvous_met` counts the first
    /// attempts that actually met, and `rendezvous_timeout` keeps a missing second
    /// request loud instead of hanging the suite.
    async fn spawn_http_retry_budget_concurrent_rejection_mock(
        rendezvous: Arc<tokio::sync::Barrier>,
        rendezvous_timeout: Duration,
        rendezvous_met: Arc<AtomicUsize>,
    ) -> (
        SocketAddr,
        Arc<Mutex<Vec<RetryBudgetConcurrencyAttempt>>>,
    ) {
        let attempts = Arc::new(Mutex::new(Vec::<RetryBudgetConcurrencyAttempt>::new()));
        let per_query_attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));

        let search_route = {
            let attempts = attempts.clone();
            let per_query_attempts = per_query_attempts.clone();
            let rendezvous = rendezvous.clone();
            let rendezvous_met = rendezvous_met.clone();
            move |headers: HeaderMap, Json(body): Json<Value>| {
                let attempts = attempts.clone();
                let per_query_attempts = per_query_attempts.clone();
                let rendezvous = rendezvous.clone();
                let rendezvous_met = rendezvous_met.clone();
                async move {
                    let api_key = body
                        .get("api_key")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    assert_eq!(
                        api_key,
                        cross_key_retry_upstream_bearer(&headers),
                        "body api_key and upstream bearer must carry the same key on /search"
                    );
                    assert!(
                        !api_key.is_empty(),
                        "upstream /search must receive an injected Tavily key"
                    );
                    let query = body
                        .get("query")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        !query.is_empty(),
                        "the concurrency mock identifies a logical request by its query"
                    );
                    let attempt = {
                        let mut counts = per_query_attempts
                            .lock()
                            .expect("per-query attempt lock poisoned");
                        let entry = counts.entry(query.clone()).or_insert(0);
                        *entry += 1;
                        *entry
                    };
                    attempts
                        .lock()
                        .expect("attempts lock poisoned")
                        .push(RetryBudgetConcurrencyAttempt {
                            api_key,
                            query: query.clone(),
                        });

                    if attempt > 1 {
                        return retry_budget_json_response(
                            StatusCode::OK,
                            retry_budget_echo_success_body(&query),
                        );
                    }
                    // Hold this first hit until the OTHER logical request's first hit
                    // has arrived too; a timeout is recorded as "did not meet" and the
                    // case asserts on the counter, so nothing can hang silently.
                    if tokio::time::timeout(rendezvous_timeout, rendezvous.wait())
                        .await
                        .is_ok()
                    {
                        rendezvous_met.fetch_add(1, Ordering::SeqCst);
                    }
                    cross_key_retry_json_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        Some("0"),
                        CROSS_KEY_RETRY_RATE_LIMITED_BODY,
                    )
                }
            }
        };

        let usage_route = || async {
            cross_key_retry_json_response(StatusCode::OK, None, CROSS_KEY_RETRY_USAGE_BODY)
        };

        let app = Router::new()
            .route("/search", post(search_route))
            .route("/usage", get(usage_route));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (addr, attempts)
    }

    /// One `observability.request_logs` row carrying the columns the concurrency case
    /// needs: the token that owns the logical request - which is what separates the two
    /// interleaved chains in one shared table - plus the attempt outcome, its marker and
    /// its key.
    #[derive(Debug)]
    struct RetryBudgetLogRow {
        id: i64,
        auth_token_id: Option<String>,
        status_code: Option<i64>,
        result_status: String,
        fallback_reason: Option<String>,
        api_key_id: Option<String>,
    }

    /// Every `request_logs` row of the database, in insert order.
    async fn fetch_retry_budget_request_log_rows(
        pool: &sqlx::SqlitePool,
    ) -> Vec<RetryBudgetLogRow> {
        let rows = sqlx::query(
            r#"
            SELECT id, auth_token_id, status_code, result_status, fallback_reason, api_key_id
            FROM observability.request_logs
            ORDER BY id ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .expect("fetch request log rows");
        rows.iter()
            .map(|row| RetryBudgetLogRow {
                id: row.try_get::<i64, _>("id").unwrap(),
                auth_token_id: row.try_get::<Option<String>, _>("auth_token_id").unwrap(),
                status_code: row.try_get::<Option<i64>, _>("status_code").unwrap(),
                result_status: row.try_get::<String, _>("result_status").unwrap(),
                fallback_reason: row
                    .try_get::<Option<String>, _>("fallback_reason")
                    .unwrap(),
                api_key_id: row.try_get::<Option<String>, _>("api_key_id").unwrap(),
            })
            .collect()
    }

    /// Longest cooldown armed for one key, as `(reason_code, seconds still pending)`.
    /// `None` means the attempt armed no cooldown at all.
    async fn fetch_retry_budget_pending_cooldown(
        pool: &sqlx::SqlitePool,
        key_id: &str,
    ) -> Option<(Option<String>, i64)> {
        let rows = sqlx::query(
            r#"
            SELECT reason_code, cooldown_until
            FROM api_key_transient_backoffs
            WHERE key_id = ?
            "#,
        )
        .bind(key_id)
        .fetch_all(pool)
        .await
        .expect("fetch transient backoff rows");
        let now = Utc::now().timestamp();
        rows.iter()
            .map(|row| {
                (
                    row.try_get::<Option<String>, _>("reason_code").unwrap(),
                    row.try_get::<i64, _>("cooldown_until").unwrap() - now,
                )
            })
            .max_by_key(|(_, pending)| *pending)
    }

    /// Assert ONE logical request's chain inside a shared log table: exactly the two
    /// rows owned by `token_id`, in attempt order, marked `attempted` -> `succeeded`,
    /// 429 then 200, on two different keys. Returns the two key ids so the caller can
    /// cross-check them against what the upstream recorder actually saw.
    fn assert_retry_budget_chain_of(
        rows: &[RetryBudgetLogRow],
        token_id: &str,
        label: &str,
    ) -> (String, String) {
        let chain: Vec<&RetryBudgetLogRow> = rows
            .iter()
            .filter(|row| row.auth_token_id.as_deref() == Some(token_id))
            .collect();
        assert_eq!(
            chain.len(),
            2,
            "{label}: one request_logs row per attempt of this chain: {chain:?}"
        );
        assert!(
            chain[0].id < chain[1].id,
            "{label}: the rows must stay in attempt order: {chain:?}"
        );
        assert_eq!(
            chain[0].status_code,
            Some(429),
            "{label}: the first hit row keeps the rejection"
        );
        assert_eq!(
            chain[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "{label}: the losing first hit is marked attempted"
        );
        assert_eq!(
            chain[1].status_code,
            Some(200),
            "{label}: the retry row is the success"
        );
        assert_eq!(
            chain[1].result_status, "success",
            "{label}: the retry row is a success"
        );
        assert_eq!(
            chain[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "{label}: the winning retry is marked succeeded"
        );
        let first_key_id = chain[0]
            .api_key_id
            .clone()
            .unwrap_or_else(|| panic!("{label}: the first attempt names its key"));
        let retry_key_id = chain[1]
            .api_key_id
            .clone()
            .unwrap_or_else(|| panic!("{label}: the retry attempt names its key"));
        assert_ne!(
            first_key_id, retry_key_id,
            "{label}: the retry must rotate to another key: {chain:?}"
        );
        (first_key_id, retry_key_id)
    }

    /// One "the first hit must rotate to another key WITHOUT spending wall clock" case:
    /// the rejection shape plus what the losing key must look like afterwards.
    struct RetryBudgetImmediateCase {
        /// Label for the QA transcript, also used as the client query.
        label: &'static str,
        /// `temp_db_path` prefix, token note and key-name stem.
        db_prefix: &'static str,
        first_status: u16,
        first_retry_after: Option<&'static str>,
        first_body: &'static str,
        /// Cooldown reason the losing key must carry, or `None` to assert this
        /// rejection arms no cooldown at all (a 432 flips the key's status instead).
        expected_backoff_reason: Option<&'static str>,
        /// `api_keys.status` the losing key must end in.
        expected_losing_key_status: &'static str,
    }

    /// Shared body of the zero-sleep battery. Every case here has a willing second key,
    /// an eligible pool and a Retry-After inside (or absent from) the kernel's fast-path
    /// window, so the only thing under test is that the rotation costs no wait: the
    /// client is served the retry's bytes inside [`RETRY_BUDGET_IMMEDIATE_BOUND`], and
    /// when the rejection arms a cooldown that cooldown is still pending by at least the
    /// same bound once the chain has returned - a sleep-based implementation would have
    /// to sit out 30s/60s/120s and cannot fake either half.
    async fn assert_first_hit_rotates_without_sleeping(case: RetryBudgetImmediateCase) {
        let db_path = temp_db_path(case.db_prefix);
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = format!("tvly-{}-a", case.db_prefix);
        let retry_key = format!("tvly-{}-b", case.db_prefix);
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                case.first_status,
                case.first_retry_after,
                case.first_body,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.clone(), retry_key.clone()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some(case.db_prefix))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        // Timed with `std::time::Instant` on purpose: the bound is about real wall
        // clock spent on the client's request, not about the executor's own deadline
        // instant (which is a `tokio::time::Instant`).
        let started = std::time::Instant::now();
        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": case.label }))
            .send()
            .await
            .expect("request to proxy succeeds");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        // The retry succeeded, so the client is never handed a Retry-After to wait on:
        // the absence of the header is the client-visible half of "no RA wait".
        assert!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .is_none(),
            "{}: a rotated request must not reach the client with a Retry-After",
            case.label
        );
        let body = resp.bytes().await.expect("read response body");
        let elapsed = started.elapsed();
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes(),
            "{}: the client must see the retry, never the buffered rejection",
            case.label
        );
        assert!(
            elapsed < RETRY_BUDGET_IMMEDIATE_BOUND,
            "{}: the chain must not sleep, yet it took {elapsed:?} (bound {:?})",
            case.label,
            RETRY_BUDGET_IMMEDIATE_BOUND
        );
        // `std::println!` on purpose: `runtime_logging` shadows the bare macro
        // crate-wide, so only this form reaches a `--nocapture` QA transcript.
        std::println!(
            "[task-8][manual-qa] {} rotated in {elapsed:?} (bound {:?}) first_status={} retry_after={:?}",
            case.label,
            RETRY_BUDGET_IMMEDIATE_BOUND,
            case.first_status,
            case.first_retry_after,
        );

        // One retry, never more, on a different key of the seeded pool.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(
            keys.len(),
            2,
            "{}: one retry, never more: {seen:?}",
            case.label
        );
        assert_ne!(
            keys[0], keys[1],
            "{}: the retry must rotate to another key",
            case.label
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            2,
            "{}: one request_logs row per attempt: {rows:?}",
            case.label
        );
        assert_eq!(
            rows[0].status_code,
            Some(i64::from(case.first_status)),
            "{}: the first hit row keeps the rejection",
            case.label
        );
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "{}: the losing first hit is marked attempted",
            case.label
        );
        assert_eq!(
            rows[1].status_code,
            Some(200),
            "{}: the retry row is the success",
            case.label
        );
        assert_eq!(
            rows[1].result_status, "success",
            "{}: the retry row is a success",
            case.label
        );
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "{}: the winning retry is marked succeeded",
            case.label
        );

        // The losing key ends exactly as its rejection family dictates - and, when that
        // marking is a cooldown, it is STILL cooling now that the chain has returned.
        let losing_key_id = rows[0]
            .api_key_id
            .clone()
            .expect("the first attempt names its key");
        assert_eq!(
            fetch_api_key_status(&pool, &losing_key_id).await,
            case.expected_losing_key_status,
            "{}: the losing key ends in the status its rejection dictates",
            case.label
        );
        let pending = fetch_retry_budget_pending_cooldown(&pool, &losing_key_id).await;
        match case.expected_backoff_reason {
            Some(reason) => {
                let (armed_reason, pending_secs) = pending.unwrap_or_else(|| {
                    panic!("{}: the losing key must be cooling", case.label)
                });
                assert_eq!(
                    armed_reason.as_deref(),
                    Some(reason),
                    "{}: the cooldown carries its family's reason code",
                    case.label
                );
                assert!(
                    pending_secs >= i64::try_from(RETRY_BUDGET_IMMEDIATE_BOUND.as_secs())
                        .expect("bound fits i64"),
                    "{}: the chain returned with {pending_secs}s of cooldown still pending, so nothing waited it out",
                    case.label
                );
                std::println!(
                    "[task-8][manual-qa] {} left {pending_secs}s of {reason} cooldown pending after {elapsed:?}",
                    case.label,
                );
            }
            None => assert!(
                pending.is_none(),
                "{}: this rejection arms no cooldown, got {:?}",
                case.label,
                pending
            ),
        }

        // Billing still settles once, on the final response.
        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "{}: one settlement per logical request, never one per attempt: {token_logs:?}",
            case.label
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].business_credits, Some(1));
        assert_eq!(token_logs[0].http_status, Some(200));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_retry_after_zero_rotates_without_sleeping() {
        // `Retry-After: 0` sits inside the kernel's 2s fast-path window. The todo-7
        // fragment pins the rotation shape for this input; this case pins that the
        // rotation costs no wall clock, and that the 30s cooldown the rejection arms
        // (RA=0 is clamped up to the ladder floor) is still pending afterwards.
        assert_first_hit_rotates_without_sleeping(RetryBudgetImmediateCase {
            label: "retry-budget-ra-zero",
            db_prefix: "http-search-retry-budget-ra-zero",
            first_status: 429,
            first_retry_after: Some("0"),
            first_body: CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            expected_backoff_reason: Some(RETRY_BUDGET_RATE_LIMIT_REASON),
            expected_losing_key_status: "active",
        })
        .await;
    }

    #[tokio::test]
    async fn tavily_http_search_missing_retry_after_rotates_without_sleeping() {
        // No Retry-After at all: the kernel's union of armed and signalled waits stays
        // empty, so the gate passes and the retry is immediate - while the armed
        // cooldown falls back to its 60s default and is still pending afterwards.
        assert_first_hit_rotates_without_sleeping(RetryBudgetImmediateCase {
            label: "retry-budget-ra-absent",
            db_prefix: "http-search-retry-budget-ra-absent",
            first_status: 429,
            first_retry_after: None,
            first_body: CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            expected_backoff_reason: Some(RETRY_BUDGET_RATE_LIMIT_REASON),
            expected_losing_key_status: "active",
        })
        .await;
    }

    #[tokio::test]
    async fn tavily_http_search_bare_401_auth_block_rotates_without_waiting_the_cooldown() {
        // The auth-block family is the strongest no-sleep pin: a bare 401 arms a >=120s
        // cooldown (plan todo 2), so an implementation that waited out what it armed
        // would need two minutes here. It must rotate immediately instead, on a key the
        // exclusion set - not the cooldown - kept available.
        assert_first_hit_rotates_without_sleeping(RetryBudgetImmediateCase {
            label: "retry-budget-bare-401",
            db_prefix: "http-search-retry-budget-bare-401",
            first_status: 401,
            first_retry_after: None,
            first_body: RETRY_BUDGET_AUTH_BLOCKED_BODY,
            expected_backoff_reason: Some(RETRY_BUDGET_AUTH_BLOCK_REASON),
            expected_losing_key_status: "active",
        })
        .await;
    }

    #[tokio::test]
    async fn tavily_http_search_432_exhaustion_rotates_without_sleeping() {
        // Exhaustion arms no cooldown but flips the key to `exhausted`, whose recovery
        // window is a month away: waiting for eligibility is not an option either, so the
        // chain must rotate immediately onto a key that is still inside its quota. The
        // todo-7 fragment pins the 432 rotation and the status flip; this case pins the
        // zero-sleep half.
        assert_first_hit_rotates_without_sleeping(RetryBudgetImmediateCase {
            label: "retry-budget-432",
            db_prefix: "http-search-retry-budget-432",
            first_status: 432,
            first_retry_after: None,
            first_body: CROSS_KEY_RETRY_EXHAUSTED_BODY,
            expected_backoff_reason: None,
            expected_losing_key_status: "exhausted",
        })
        .await;
    }

    /// The deadline gate. A wall-clock timeout is NOT injectable at this seam - the
    /// shared direct client is built without `.timeout(..)`
    /// (`src/forward_proxy/relay_and_xray.rs::build_direct_client`) and the executor
    /// consults the deadline BETWEEN attempts - so the only way to spend the 5s budget is
    /// an upstream that is slow but finite. Hence the delayed mock above: it answers 429
    /// after [`RETRY_BUDGET_SLOW_FIRST_HIT`], by which time
    /// `retry_deadline_start.elapsed()` has passed the deadline and the kernel must
    /// answer `ServeOriginal(DeadlineExceeded)`.
    #[tokio::test]
    async fn tavily_http_search_slow_first_hit_spends_the_retry_deadline_and_is_served() {
        let db_path = temp_db_path("http-search-retry-budget-deadline");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-retry-budget-deadline-a";
        let retry_key = "tvly-retry-budget-deadline-b";
        let (upstream_addr, attempts) =
            spawn_http_retry_budget_slow_first_hit_mock(RETRY_BUDGET_SLOW_FIRST_HIT).await;
        let usage_base = format!("http://{upstream_addr}");

        // A two-key pool: the second key stays eligible the whole time, so ONLY the
        // deadline can explain the missing second attempt.
        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-retry-budget-deadline"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let started = std::time::Instant::now();
        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "retry budget deadline" }))
            .send()
            .await
            .expect("request to proxy succeeds");
        let elapsed = started.elapsed();

        // The buffered first hit is served byte-for-byte: status, the upstream
        // Retry-After and content type, and the body bytes with a matching length. The
        // 502 path is never reused for a spent deadline.
        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the upstream Retry-After header must be preserved"
        );
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "the upstream content type must be preserved"
        );
        let expected_len = CROSS_KEY_RETRY_RATE_LIMITED_BODY.len().to_string();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_len.as_str()),
            "the passthrough content length must match the passthrough bytes"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes(),
            "the slow first hit must reach the client byte-for-byte"
        );

        // Non-vacuity: the first hit really did outlast the deadline, so the gate - and
        // not a missing retry path - is what this case observed.
        assert!(
            elapsed >= RETRY_BUDGET_DEADLINE,
            "the first hit must outlast the {RETRY_BUDGET_DEADLINE:?} deadline, took {elapsed:?}"
        );
        std::println!(
            "[task-8][manual-qa] deadline case: first hit answered after {elapsed:?} (deadline {RETRY_BUDGET_DEADLINE:?}, mock delay {RETRY_BUDGET_SLOW_FIRST_HIT:?})"
        );

        // No second upstream call was ever made. WHICH seeded key took the single
        // attempt is not deterministic - `acquire_key_for` orders equal candidates by
        // `id ASC` and ids are random - so this pins the attempt COUNT and that the key
        // came from the seeded pool, and derives the untouched key below by exclusion.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let seen_keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(
            seen_keys.len(),
            1,
            "a spent deadline must not start a second attempt: {seen:?}"
        );
        assert!(
            [first_key, retry_key].contains(&seen_keys[0].as_str()),
            "only a seeded pool key may reach upstream: {seen:?}"
        );
        std::println!(
            "[task-8][manual-qa] deadline case: the single attempt ran on {seen_keys:?} of a two-key pool, so only the deadline explains the missing retry"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            1,
            "the slow attempt keeps its single visible row: {rows:?}"
        );
        assert_eq!(rows[0].status_code, Some(429));
        assert_eq!(rows[0].result_status, "error");
        // The deadline arm serves the buffered hit UNMARKED: `skipped_no_eligible_key`
        // belongs to the pool arm alone, and an operator must be able to tell "the pool
        // was empty" from "the budget ran out" (the latter shows up as a slow single
        // attempt with no chain marker).
        assert!(
            rows[0].fallback_reason.is_none(),
            "a spent deadline carries no retry-chain marker, got {:?}",
            rows[0].fallback_reason
        );

        // The key that was never tried stays eligible - proof the pool, not its own
        // state, was ready for a retry the deadline refused.
        let used_key_id = rows[0]
            .api_key_id
            .clone()
            .expect("the slow attempt names its key");
        let untouched_secret = if fetch_api_key_id_by_secret(&pool, first_key).await == used_key_id {
            retry_key
        } else {
            first_key
        };
        let untouched_key_id = fetch_api_key_id_by_secret(&pool, untouched_secret).await;
        assert_eq!(fetch_api_key_status(&pool, &untouched_key_id).await, "active");
        assert!(
            fetch_armed_transient_backoffs(&pool, &untouched_key_id)
                .await
                .is_empty(),
            "the untouched key must still be eligible for a retry the deadline refused"
        );

        // Serving the first hit still settles the logical request exactly once.
        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(429));

        let _ = std::fs::remove_file(db_path);
    }

    /// Two concurrent logical requests, both rejected, both rotating - without cross
    /// talk. Concurrency is forced, not hoped for: the mock holds each request's first
    /// hit on a two-party barrier until the other request's first hit has arrived too,
    /// and the case asserts both parties met.
    #[tokio::test]
    async fn tavily_http_search_two_concurrent_rejections_rotate_without_cross_talk() {
        let db_path = temp_db_path("http-search-retry-budget-concurrent");
        let db_str = db_path.to_string_lossy().to_string();

        // Four keys, not two: both logical requests lose their first key to a 429
        // cooldown at the same instant and the strict acquire skips cooling keys, so a
        // two-key pool could legitimately run out of eligible keys mid-race and the case
        // would end up pinning the pool instead of the concurrency. Four keeps "each
        // chain finds another key" deterministic; the attempt ceiling has its own case
        // and does not depend on this pool size.
        let pool_keys: Vec<String> = (0..4)
            .map(|index| format!("tvly-retry-budget-concurrent-k{index}"))
            .collect();

        let rendezvous = Arc::new(tokio::sync::Barrier::new(2));
        let rendezvous_met = Arc::new(AtomicUsize::new(0));
        let (upstream_addr, attempts) = spawn_http_retry_budget_concurrent_rejection_mock(
            rendezvous.clone(),
            // Bounded so a request that never reaches upstream fails the case instead of
            // hanging the suite.
            Duration::from_secs(30),
            rendezvous_met.clone(),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(pool_keys.clone(), DEFAULT_UPSTREAM, &db_str)
            .await
            .expect("proxy created");
        let token_a = proxy
            .create_access_token(Some("http-search-retry-budget-concurrent-a"))
            .await
            .expect("create token a");
        let token_b = proxy
            .create_access_token(Some("http-search-retry-budget-concurrent-b"))
            .await
            .expect("create token b");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let url = format!("http://{proxy_addr}/api/tavily/search");
        let client = Client::new();
        // Two DIFFERENT tokens on purpose: `TavilyProxy::lock_token_billing` holds a
        // per-token lock across the upstream call, so two requests sharing one token
        // serialize and never actually overlap (that serialization is pinned by
        // tavily_http_search_concurrent_requests_do_not_bypass_quota_due_to_billing_lock).
        let request_a = {
            let client = client.clone();
            let url = url.clone();
            let secret = token_a.token.clone();
            async move {
                client
                    .post(&url)
                    .header("Authorization", format!("Bearer {secret}"))
                    .json(&serde_json::json!({ "query": RETRY_BUDGET_CONCURRENT_QUERY_A }))
                    .send()
                    .await
                    .expect("request a to proxy succeeds")
            }
        };
        let request_b = {
            let client = client.clone();
            let url = url.clone();
            let secret = token_b.token.clone();
            async move {
                client
                    .post(&url)
                    .header("Authorization", format!("Bearer {secret}"))
                    .json(&serde_json::json!({ "query": RETRY_BUDGET_CONCURRENT_QUERY_B }))
                    .send()
                    .await
                    .expect("request b to proxy succeeds")
            }
        };
        let (resp_a, resp_b) = tokio::join!(request_a, request_b);

        // Each client saw its OWN chain's success: the mock echoes the request's query
        // into the body, so a crossed chain would hand A the bytes of B.
        assert_eq!(resp_a.status(), reqwest::StatusCode::OK);
        assert_eq!(resp_b.status(), reqwest::StatusCode::OK);
        let body_a = resp_a.bytes().await.expect("read response body a");
        let body_b = resp_b.bytes().await.expect("read response body b");
        assert_eq!(
            body_a.as_ref(),
            retry_budget_echo_success_body(RETRY_BUDGET_CONCURRENT_QUERY_A).as_slice(),
            "request a must be served its own chain's bytes"
        );
        assert_eq!(
            body_b.as_ref(),
            retry_budget_echo_success_body(RETRY_BUDGET_CONCURRENT_QUERY_B).as_slice(),
            "request b must be served its own chain's bytes"
        );

        assert_eq!(
            rendezvous_met.load(Ordering::SeqCst),
            2,
            "both first hits must have been in flight at the same instant, otherwise this \
             case never exercised concurrency at all"
        );

        // Upstream saw exactly four attempts: two per logical request, each pair on two
        // different keys. The two chains MAY share a retry key (both may land on the same
        // still-eligible key), so distinctness is asserted within a chain only.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            seen.len(),
            4,
            "two logical requests x two attempts each: {seen:?}"
        );
        for (label, query) in [
            ("a", RETRY_BUDGET_CONCURRENT_QUERY_A),
            ("b", RETRY_BUDGET_CONCURRENT_QUERY_B),
        ] {
            let chain: Vec<&RetryBudgetConcurrencyAttempt> = seen
                .iter()
                .filter(|attempt| attempt.query == query)
                .collect();
            assert_eq!(
                chain.len(),
                2,
                "{label}: exactly two upstream attempts: {seen:?}"
            );
            assert_ne!(
                chain[0].api_key, chain[1].api_key,
                "{label}: the retry must rotate to another key: {seen:?}"
            );
            assert!(
                chain
                    .iter()
                    .all(|attempt| pool_keys.contains(&attempt.api_key)),
                "{label}: only seeded pool keys may reach upstream: {seen:?}"
            );
        }

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_retry_budget_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            4,
            "one request_logs row per attempt of either chain: {rows:?}"
        );
        let chain_a = assert_retry_budget_chain_of(&rows, &token_a.id, "a");
        let chain_b = assert_retry_budget_chain_of(&rows, &token_b.id, "b");

        // The log rows and the upstream recorder must describe the SAME two chains, in
        // the same order: each token's logged key ids are exactly the keys its own query
        // reached upstream with. A crossed or shared chain would show up here.
        for (label, query, logged) in [
            ("a", RETRY_BUDGET_CONCURRENT_QUERY_A, chain_a),
            ("b", RETRY_BUDGET_CONCURRENT_QUERY_B, chain_b),
        ] {
            let mut recorded: Vec<String> = Vec::new();
            for attempt in seen.iter().filter(|attempt| attempt.query == query) {
                recorded.push(fetch_api_key_id_by_secret(&pool, &attempt.api_key).await);
            }
            assert_eq!(
                recorded,
                vec![logged.0.clone(), logged.1.clone()],
                "{label}: the logged chain must match the attempts this query actually made"
            );
        }

        // Billing: one settlement per logical request, so exactly two rows in the whole
        // table - never one per attempt (which would be four).
        for (label, token) in [("a", &token_a), ("b", &token_b)] {
            let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
            assert_eq!(
                token_logs.len(),
                1,
                "{label}: one settlement per logical request, never one per attempt: {token_logs:?}"
            );
            assert_eq!(token_logs[0].result_status, "success");
            assert_eq!(token_logs[0].business_credits, Some(1));
            assert_eq!(token_logs[0].http_status, Some(200));
        }
        let total_settlements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_token_logs")
            .fetch_one(&pool)
            .await
            .expect("count auth_token_logs rows");
        assert_eq!(
            total_settlements, 2,
            "two concurrent logical requests settle exactly twice"
        );
        std::println!(
            "[task-8][manual-qa] concurrency case: rendezvous_met={} upstream_attempts={} request_log_rows={} settlements={}",
            rendezvous_met.load(Ordering::SeqCst),
            seen.len(),
            rows.len(),
            total_settlements,
        );

        let _ = std::fs::remove_file(db_path);
    }

    /// The attempt ceiling is a BUDGET, not pool exhaustion: with three keys that all
    /// reject, the chain still stops after two upstream calls and the third key is never
    /// touched, marked or cooled. The todo-7 fragment pins the terminal `rejected`
    /// marker on a two-key pool, where "the pool ran out" and "the budget ran out" are
    /// indistinguishable; this case separates them.
    #[tokio::test]
    async fn tavily_http_search_attempt_ceiling_holds_when_the_pool_is_larger() {
        let db_path = temp_db_path("http-search-retry-budget-ceiling");
        let db_str = db_path.to_string_lossy().to_string();

        let pool_keys: Vec<String> = (0..3)
            .map(|index| format!("tvly-retry-budget-ceiling-k{index}"))
            .collect();
        // Every attempt rejects with RA=0, so the second key is willing and eligible and
        // a third one is sitting right there: only the 2-attempt budget can stop the
        // chain where it does.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rejecting_every_attempt(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(pool_keys.clone(), DEFAULT_UPSTREAM, &db_str)
            .await
            .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-retry-budget-ceiling"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "retry budget ceiling" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        // The client gets the FINAL attempt byte-for-byte, still the upstream rejection.
        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the final attempt's Retry-After header must survive"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes());

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(
            keys.len(),
            2,
            "the budget, not the pool, ends the chain: {seen:?}"
        );
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");

        let pool = connect_sqlite_test_pool(&db_str).await;
        // The third key never reached upstream and stays pristine: no status change, no
        // cooldown. Burning the rest of the pool is exactly what the ceiling prevents.
        let untouched_secret = pool_keys
            .iter()
            .find(|key| !keys.contains(key))
            .expect("a three-key pool leaves one key unused after two attempts");
        let untouched_key_id = fetch_api_key_id_by_secret(&pool, untouched_secret).await;
        assert_eq!(
            fetch_api_key_status(&pool, &untouched_key_id).await,
            "active",
            "the key the ceiling never reached must stay active"
        );
        assert!(
            fetch_armed_transient_backoffs(&pool, &untouched_key_id)
                .await
                .is_empty(),
            "the key the ceiling never reached must not be cooled either"
        );

        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            2,
            "one request_logs row per attempt, and no row for the key never tried: {rows:?}"
        );
        assert_eq!(rows[0].status_code, Some(429));
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the losing first hit is marked attempted"
        );
        assert_eq!(rows[1].status_code, Some(429));
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:rejected"),
            "a failed retry terminates the chain as rejected"
        );
        std::println!(
            "[task-8][manual-qa] ceiling case: upstream_attempts={} pool_size={} untouched_key={untouched_secret} markers={:?}->{:?}",
            seen.len(),
            pool_keys.len(),
            rows[0].fallback_reason,
            rows[1].fallback_reason,
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "error");
        assert_eq!(
            token_logs[0].http_status,
            Some(429),
            "the settlement records the final attempt"
        );

        let _ = std::fs::remove_file(db_path);
    }
