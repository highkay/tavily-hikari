    // Bounded cross-key retry seam coverage (plan provider-key-429-retry todo 6,
    // extended by the todo 7 integration battery).
    //
    // This fragment is `include!`d from `tavily_http_search.rs` because that file
    // already sits at its `rust_source_line_budgets` ceiling (3219/3250) and the
    // plan forbids raising a budget. `include!` keeps the module path - and with
    // it the CI manifest prefix `server::tests::tavily_http_search::tavily_` -
    // unchanged, so no manifest edit is needed either. rustfmt never visits an
    // include!-only file, so the four-space module indentation below is kept by
    // hand.
    //
    // What the battery pins, per the 2026-10-16 amendment:
    // * an explicit rejection (429 RA=0, 432) rotates ONCE to another pool key
    //   and the client sees only the retry's bytes;
    // * a key-scoped transient failure (upstream 5xx, a transport send failure)
    //   rotates the same way - rotation there comes from the exclusion set, not
    //   from cooling, because a 5xx arms no backoff;
    // * a request-shaped 4xx (400/422) is served as-is on a SINGLE attempt with
    //   the key neither marked nor cooled and no chain marker;
    // * `/research` POST, `/research/{id}` GET and `/crawl` are never replayed:
    //   the surface exclusion beats eligibility even with a willing second key;
    // * a pool with no eligible key serves the buffered first hit byte-for-byte
    //   (status, Retry-After, content type, body bytes);
    // * a plain success never enters the chain, and billing settles exactly once
    //   on whatever the chain returns.

    /// Body the mock returns when it rejects an attempt as rate limited.
    const CROSS_KEY_RETRY_RATE_LIMITED_BODY: &str = r#"{"error":"rate limited"}"#;
    /// 432 rejection body: Tavily's monthly run-out, the exhaustion isomorph of
    /// the 429 case.
    const CROSS_KEY_RETRY_EXHAUSTED_BODY: &str = r#"{"error":"monthly quota exhausted"}"#;
    /// Upstream 5xx body: the amendment's key-scoped transient arm.
    const CROSS_KEY_RETRY_SERVER_ERROR_BODY: &str = r#"{"error":"upstream exploded"}"#;
    /// Request-shaped 4xx body: rotating a key cannot fix a wrong request.
    const CROSS_KEY_RETRY_REQUEST_ERROR_BODY: &str = r#"{"error":"invalid request payload"}"#;
    /// Body the mock returns on every attempt after the first one. Distinct from
    /// the rejection body so the client-visible bytes prove WHICH attempt was
    /// served.
    const CROSS_KEY_RETRY_SECOND_HIT_BODY: &str =
        r#"{"status":200,"results":[{"title":"second key hit"}],"usage":{"credits":1}}"#;
    /// `/usage` probe body. The research POST path may probe key usage, and a
    /// probe must never be mistaken for a business attempt.
    const CROSS_KEY_RETRY_USAGE_BODY: &str = r#"{"key":{"research_usage":10}}"#;

    /// One upstream attempt as the mock saw it: the route path and the pool key
    /// that reached upstream. Recording both is what lets a case prove the chain
    /// rotated to another key - or that it never started.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CrossKeyRetryAttempt {
        path: String,
        api_key: String,
    }

    /// The upstream keys an attempt recorder saw, in order.
    fn cross_key_retry_seen_keys(attempts: &[CrossKeyRetryAttempt]) -> Vec<String> {
        attempts
            .iter()
            .map(|attempt| attempt.api_key.clone())
            .collect()
    }

    /// The recorded attempts that hit one exact upstream path.
    fn cross_key_retry_attempts_on<'a>(
        attempts: &'a [CrossKeyRetryAttempt],
        path: &str,
    ) -> Vec<&'a CrossKeyRetryAttempt> {
        attempts
            .iter()
            .filter(|attempt| attempt.path == path)
            .collect()
    }

    /// Which route the shared mock rejects on its first attempt.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CrossKeyRetryRejectedRoute {
        /// Nothing rejects: every attempt answers 200. The success-path pin uses
        /// this, and so does the transport case - whose first attempt dies below
        /// HTTP, before the mock can answer anything.
        Never,
        /// A JSON POST route: `/search`, `/extract`, `/crawl`, `/map`, `/research`.
        JsonPost(&'static str),
        /// The `GET /research/{request_id}` result poll.
        ResearchResultGet,
    }

    /// Mock behaviour: which route rejects, with what first-attempt response, and
    /// whether later attempts on that route succeed.
    #[derive(Debug, Clone, Copy)]
    struct CrossKeyRetryMockPlan {
        rejected_route: CrossKeyRetryRejectedRoute,
        first_status: StatusCode,
        first_retry_after: Option<&'static str>,
        first_body: &'static str,
        later_attempts_succeed: bool,
    }

    impl CrossKeyRetryMockPlan {
        /// The rotation shape: the first attempt on `route` answers `first_status`
        /// and `first_body` (plus a Retry-After header when given), and every
        /// later attempt on it succeeds. A second eligible key exists and the
        /// mock is willing, so ONLY the retry policy explains what the client
        /// ends up seeing.
        fn rotating_after_rejection(
            route: CrossKeyRetryRejectedRoute,
            first_status: u16,
            first_retry_after: Option<&'static str>,
            first_body: &'static str,
        ) -> Self {
            Self::new(
                route,
                first_status,
                first_retry_after,
                first_body,
                true,
            )
        }

        /// Every attempt on `route` answers the same rejection: the chain has a
        /// second key to try and still loses, which pins the terminal marker and
        /// the one-retry cap.
        fn rejecting_every_attempt(
            route: CrossKeyRetryRejectedRoute,
            first_status: u16,
            first_retry_after: Option<&'static str>,
            first_body: &'static str,
        ) -> Self {
            Self::new(
                route,
                first_status,
                first_retry_after,
                first_body,
                false,
            )
        }

        /// No route ever rejects.
        fn all_success() -> Self {
            Self::new(
                CrossKeyRetryRejectedRoute::Never,
                200,
                None,
                CROSS_KEY_RETRY_SECOND_HIT_BODY,
                true,
            )
        }

        fn new(
            route: CrossKeyRetryRejectedRoute,
            first_status: u16,
            first_retry_after: Option<&'static str>,
            first_body: &'static str,
            later_attempts_succeed: bool,
        ) -> Self {
            Self {
                rejected_route: route,
                first_status: StatusCode::from_u16(first_status).expect("valid mock status"),
                first_retry_after,
                first_body,
                later_attempts_succeed,
            }
        }

        /// True when `path` is the route this plan rejects on.
        fn rejects(&self, path: &str) -> bool {
            match self.rejected_route {
                CrossKeyRetryRejectedRoute::Never => false,
                CrossKeyRetryRejectedRoute::JsonPost(route) => path == route,
                // The result poll carries the request id in its path.
                CrossKeyRetryRejectedRoute::ResearchResultGet => path.starts_with("/research/"),
            }
        }

        /// Response for the `attempt`-th hit (1-based) on `path`.
        fn response_for(&self, path: &str, attempt: usize) -> Response {
            if path == "/usage" {
                return cross_key_retry_json_response(
                    StatusCode::OK,
                    None,
                    CROSS_KEY_RETRY_USAGE_BODY,
                );
            }
            if self.rejects(path) && (attempt == 1 || !self.later_attempts_succeed) {
                return cross_key_retry_json_response(
                    self.first_status,
                    self.first_retry_after,
                    self.first_body,
                );
            }
            cross_key_retry_json_response(StatusCode::OK, None, CROSS_KEY_RETRY_SECOND_HIT_BODY)
        }
    }

    /// JSON response with an optional Retry-After header.
    fn cross_key_retry_json_response(
        status: StatusCode,
        retry_after: Option<&'static str>,
        body: &'static str,
    ) -> Response {
        let mut builder = Response::builder()
            .status(status)
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(retry_after) = retry_after {
            builder = builder.header(axum::http::header::RETRY_AFTER, retry_after);
        }
        builder.body(Body::from(body)).expect("build mock response")
    }

    /// Record one attempt and return how many hits this path has seen (1 first).
    fn cross_key_retry_record_attempt(
        attempts: &Arc<Mutex<Vec<CrossKeyRetryAttempt>>>,
        per_path: &Arc<Mutex<HashMap<String, usize>>>,
        path: &str,
        api_key: &str,
    ) -> usize {
        let attempt = {
            let mut counts = per_path.lock().expect("per-path attempt lock poisoned");
            let entry = counts.entry(path.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        attempts
            .lock()
            .expect("attempts lock poisoned")
            .push(CrossKeyRetryAttempt {
                path: path.to_string(),
                api_key: api_key.to_string(),
            });
        attempt
    }

    /// Bearer token of an upstream request ("" when absent).
    fn cross_key_retry_upstream_bearer(headers: &HeaderMap) -> String {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_string()
    }

    /// Router serving the whole Tavily HTTP surface under `plan` and recording
    /// every attempt: the JSON POST routes the executor injects `api_key` into,
    /// plus the two bearer-only GET routes. Registering all of them is what lets
    /// one mock serve the search, crawl and research cases alike.
    fn cross_key_retry_mock_router(
        plan: CrossKeyRetryMockPlan,
        attempts: Arc<Mutex<Vec<CrossKeyRetryAttempt>>>,
    ) -> Router {
        let per_path_attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));

        let json_post_route = |route: &'static str| {
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
                        "body api_key and upstream bearer must carry the same key on {route}"
                    );
                    assert!(
                        !api_key.is_empty(),
                        "upstream {route} must receive an injected Tavily key"
                    );
                    let path = uri.path().to_string();
                    let attempt = cross_key_retry_record_attempt(
                        &attempts,
                        &per_path_attempts,
                        &path,
                        &api_key,
                    );
                    plan.response_for(&path, attempt)
                }
            }
        };

        let research_result_route = {
            let attempts = attempts.clone();
            let per_path_attempts = per_path_attempts.clone();
            move |uri: Uri,
                  headers: HeaderMap,
                  axum::extract::Path(request_id): axum::extract::Path<String>| {
                let attempts = attempts.clone();
                let per_path_attempts = per_path_attempts.clone();
                async move {
                    let api_key = cross_key_retry_upstream_bearer(&headers);
                    assert!(
                        !api_key.is_empty(),
                        "upstream /research/:request_id must receive an injected Tavily bearer"
                    );
                    let path = uri.path().to_string();
                    assert!(
                        path.ends_with(&request_id),
                        "upstream result path {path} must carry the request id"
                    );
                    let attempt = cross_key_retry_record_attempt(
                        &attempts,
                        &per_path_attempts,
                        &path,
                        &api_key,
                    );
                    plan.response_for(&path, attempt)
                }
            }
        };

        // A usage probe is not a business attempt: it answers, but is not
        // recorded, so the attempt recorder stays a faithful upstream-call count.
        let usage_route = || async {
            cross_key_retry_json_response(StatusCode::OK, None, CROSS_KEY_RETRY_USAGE_BODY)
        };

        Router::new()
            .route("/search", post(json_post_route("/search")))
            .route("/extract", post(json_post_route("/extract")))
            .route("/crawl", post(json_post_route("/crawl")))
            .route("/map", post(json_post_route("/map")))
            .route("/research", post(json_post_route("/research")))
            .route("/research/:request_id", get(research_result_route))
            .route("/usage", get(usage_route))
    }

    /// Shared mock Tavily upstream for the cross-key retry battery: binds an
    /// ephemeral port and serves [`cross_key_retry_mock_router`].
    async fn spawn_http_cross_key_retry_mock(
        plan: CrossKeyRetryMockPlan,
    ) -> (SocketAddr, Arc<Mutex<Vec<CrossKeyRetryAttempt>>>) {
        let attempts = Arc::new(Mutex::new(Vec::<CrossKeyRetryAttempt>::new()));
        let app = cross_key_retry_mock_router(plan, attempts.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (addr, attempts)
    }

    /// Mock whose FIRST accepted TCP connection is answered with a malformed
    /// status line and closed, so reqwest fails that attempt below HTTP; every
    /// later connection is served normally (200 + the second-hit body). A
    /// successful retry therefore proves the seam rotated keys on transport
    /// evidence alone.
    ///
    /// A wall-clock TIMEOUT is deliberately not injected: the shared direct
    /// client is built without `.timeout(..)`
    /// (`src/forward_proxy/relay_and_xray.rs::build_direct_client`), so a slow
    /// mock would hang the attempt instead of failing it, and the retry deadline
    /// is only consulted between attempts. `KeyRetryTransportFailure::Timeout` is
    /// pinned where it is decided - the kernel table row
    /// `proxy_key_retry::transport_timeout_evidence_retries` - while this case
    /// pins the SEAM, which funnels every `ProxyError::Http` transport failure
    /// through the same `key_retry_transport_evidence` mapping. hyper-util's
    /// legacy client cannot hide the injected failure either: it only replays a
    /// request whose connection was REUSED from the pool
    /// (`client/legacy/client.rs::send_request` - "a fresh connection means we
    /// definitely can't retry"), so a first-attempt fault always reaches the seam.
    ///
    /// What the seam then sees depends on whether an HTTP proxy sits in front of
    /// the mock, which the environment decides and the test does not control
    /// (`build_direct_client` keeps reqwest's default proxy detection):
    /// * direct connection - reqwest fails below HTTP, the seam takes its
    ///   transport arm and the attempt's row carries NO status;
    /// * a transparent proxy (this repo's dev WSL exports `http_proxy`) - the
    ///   poison kills the proxy's upstream leg, so the proxy answers 502 and the
    ///   seam takes its buffered-5xx arm instead.
    ///
    /// Both are the amendment's key-scoped transient arm and both must rotate,
    /// so the case pins the rotation and tolerates exactly those two shapes.
    async fn spawn_http_search_mock_failing_first_attempt_transport(
    ) -> (
        SocketAddr,
        Arc<Mutex<Vec<CrossKeyRetryAttempt>>>,
        Arc<AtomicUsize>,
    ) {
        let attempts = Arc::new(Mutex::new(Vec::<CrossKeyRetryAttempt>::new()));
        let transport_failures = Arc::new(AtomicUsize::new(0));
        let app =
            cross_key_retry_mock_router(CrossKeyRetryMockPlan::all_success(), attempts.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let spawned_transport_failures = transport_failures.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            // Deterministic ordering: poison exactly ONE accepted connection
            // before handing the listener to axum, so the first attempt can only
            // fail on the transport layer and every later attempt is served.
            if let Ok((mut stream, _peer)) = listener.accept().await {
                let _ = stream.write_all(b"NOT-AN-HTTP-RESPONSE\r\n\r\n").await;
                let _ = stream.shutdown().await;
                spawned_transport_failures.fetch_add(1, Ordering::SeqCst);
            }
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (addr, attempts, transport_failures)
    }

    /// One `observability.request_logs` row of a Tavily HTTP attempt, in the
    /// order the attempts ran. `path` is the client-facing display path, so a
    /// case that also probes upstream can still isolate its own rows.
    #[derive(Debug)]
    struct CrossKeyRetryLogRow {
        path: String,
        status_code: Option<i64>,
        result_status: String,
        fallback_reason: Option<String>,
        api_key_id: Option<String>,
        key_effect_code: String,
    }

    /// The retry chain's `request_logs` rows in attempt order.
    async fn fetch_cross_key_retry_request_log_rows(
        pool: &sqlx::SqlitePool,
    ) -> Vec<CrossKeyRetryLogRow> {
        let rows = sqlx::query(
            r#"
            SELECT path, status_code, result_status, fallback_reason, api_key_id, key_effect_code
            FROM observability.request_logs
            ORDER BY id ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .expect("fetch request log rows");
        rows.iter()
            .map(|row| CrossKeyRetryLogRow {
                path: row.try_get::<String, _>("path").unwrap(),
                status_code: row.try_get::<Option<i64>, _>("status_code").unwrap(),
                result_status: row.try_get::<String, _>("result_status").unwrap(),
                fallback_reason: row
                    .try_get::<Option<String>, _>("fallback_reason")
                    .unwrap(),
                api_key_id: row.try_get::<Option<String>, _>("api_key_id").unwrap(),
                key_effect_code: row.try_get::<String, _>("key_effect_code").unwrap(),
            })
            .collect()
    }

    /// One `auth_token_logs` row. Billing settles once per logical request, so a
    /// retry chain must never produce more than one - and it must describe the
    /// FINAL attempt.
    #[derive(Debug)]
    struct CrossKeyRetryTokenLog {
        result_status: String,
        business_credits: Option<i64>,
        http_status: Option<i64>,
    }

    async fn fetch_cross_key_retry_token_logs(
        pool: &sqlx::SqlitePool,
        token_id: &str,
    ) -> Vec<CrossKeyRetryTokenLog> {
        let rows = sqlx::query(
            r#"
            SELECT result_status, business_credits, http_status
            FROM auth_token_logs
            WHERE token_id = ?
            ORDER BY id ASC
            "#,
        )
        .bind(token_id)
        .fetch_all(pool)
        .await
        .expect("fetch token log rows");
        rows.iter()
            .map(|row| CrossKeyRetryTokenLog {
                result_status: row.try_get::<String, _>("result_status").unwrap(),
                business_credits: row.try_get::<Option<i64>, _>("business_credits").unwrap(),
                http_status: row.try_get::<Option<i64>, _>("http_status").unwrap(),
            })
            .collect()
    }

    /// `api_keys.id` of one seeded secret.
    async fn fetch_api_key_id_by_secret(pool: &sqlx::SqlitePool, secret: &str) -> String {
        sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ?")
            .bind(secret)
            .fetch_one(pool)
            .await
            .expect("fetch api key id")
    }

    /// `api_keys.status` of one key.
    async fn fetch_api_key_status(pool: &sqlx::SqlitePool, key_id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM api_keys WHERE id = ?")
            .bind(key_id)
            .fetch_one(pool)
            .await
            .expect("fetch api key status")
    }

    /// Transient backoffs armed for one key as `(scope, reason_code)`. Empty
    /// means the key was never cooled.
    async fn fetch_armed_transient_backoffs(
        pool: &sqlx::SqlitePool,
        key_id: &str,
    ) -> Vec<(String, Option<String>)> {
        let rows = sqlx::query(
            r#"
            SELECT scope, reason_code
            FROM api_key_transient_backoffs
            WHERE key_id = ?
            ORDER BY scope ASC
            "#,
        )
        .bind(key_id)
        .fetch_all(pool)
        .await
        .expect("fetch transient backoff rows");
        rows.iter()
            .map(|row| {
                (
                    row.try_get::<String, _>("scope").unwrap(),
                    row.try_get::<Option<String>, _>("reason_code").unwrap(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_serves_the_second_key_success() {
        let db_path = temp_db_path("http-search-cross-key-retry");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-a";
        let retry_key = "tvly-cross-key-retry-b";
        // Retry-After: 0 sits inside the kernel's 2s fast-path window, so the
        // retry is immediate and never sleeps.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        // The client sees the RETRY's response byte-for-byte; the buffered 429
        // never reaches it.
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes());

        // Exactly one extra attempt, on a DIFFERENT key of the seeded pool.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(keys.len(), 2, "one retry, never more: {seen:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");
        assert!(
            keys.iter()
                .all(|key| key == first_key || key == retry_key),
            "only seeded pool keys may reach upstream: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;

        // One visible request_logs row per attempt, marked attempted -> succeeded,
        // and the rotation is visible in the rows too: each attempt names its own
        // key, so an operator can see WHICH key lost and which one served.
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].status_code, Some(429), "first hit row keeps the 429");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the losing first hit is marked attempted"
        );
        assert_eq!(rows[1].status_code, Some(200), "second hit row is the success");
        assert_eq!(rows[1].result_status, "success");
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "the winning retry is marked succeeded"
        );
        let (first_key_id, retry_key_id) = (
            rows[0].api_key_id.clone().expect("first attempt names its key"),
            rows[1].api_key_id.clone().expect("retry attempt names its key"),
        );
        assert_ne!(
            first_key_id, retry_key_id,
            "the two rows must name two different pool keys: {rows:?}"
        );
        assert_eq!(
            first_key_id,
            fetch_api_key_id_by_secret(&pool, &keys[0]).await,
            "the losing row names the key that actually hit upstream first"
        );
        assert_eq!(
            retry_key_id,
            fetch_api_key_id_by_secret(&pool, &keys[1]).await,
            "the winning row names the key that served the retry"
        );

        // Billing settles ONCE, on the final response: a single auth_token_logs
        // row charging the second hit's usage.
        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(
            token_logs[0].business_credits,
            Some(1),
            "the retry is not billed twice"
        );
        assert_eq!(token_logs[0].http_status, Some(200));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_serves_first_hit_bytes_without_eligible_key() {
        let db_path = temp_db_path("http-search-cross-key-retry-empty-pool");
        let db_str = db_path.to_string_lossy().to_string();

        let only_key = "tvly-cross-key-retry-only";
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        // A single-key pool: the rejected key is the whole pool, so the strict
        // acquire has nothing left to offer.
        let proxy = TavilyProxy::with_endpoint(vec![only_key.to_string()], DEFAULT_UPSTREAM, &db_str)
            .await
            .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-empty-pool"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry empty pool" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        // Byte-for-byte passthrough of the buffered first hit: status, the
        // upstream Retry-After and content type headers, the body bytes and the
        // content length all survive, and the 502 path is never reused.
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
        let expected_passthrough_len = CROSS_KEY_RETRY_RATE_LIMITED_BODY.len().to_string();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_passthrough_len.as_str()),
            "the passthrough content length must match the passthrough bytes"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes(),
            "the buffered first hit must reach the client byte-for-byte"
        );

        // No second upstream call was ever made.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            cross_key_retry_seen_keys(&seen),
            vec![only_key.to_string()],
            "single attempt only: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].status_code, Some(429));
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "an empty pool is marked, not silently retried"
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

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_marks_the_final_rejection_and_stops() {
        let db_path = temp_db_path("http-search-cross-key-retry-rejected");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-reject-a";
        let retry_key = "tvly-cross-key-retry-reject-b";
        // Both keys are rate limited, so the retry fails too: this pins the
        // terminal `rejected` marker AND the hard cap of one extra attempt.
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

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-rejected"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry both rejected" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        // The client gets the FINAL attempt byte-for-byte, still the upstream
        // rejection - the chain is never turned into a 502.
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

        // Hard cap: the first attempt plus ONE retry, on two different keys.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(keys.len(), 2, "never a third upstream attempt: {seen:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
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

        // One token log per logical request even though two attempts ran, and it
        // describes the FINAL attempt: the retry never re-enters dispatch,
        // reservation or settlement.
        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one token log per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(
            token_logs[0].result_status, "error",
            "a rejected chain is logged as the failure it ended with"
        );
        assert_eq!(
            token_logs[0].http_status,
            Some(429),
            "the token log records the final attempt, not the first hit"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_refuses_a_long_retry_after() {
        let db_path = temp_db_path("http-search-cross-key-retry-long-ra");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-long-ra-a";
        let retry_key = "tvly-cross-key-retry-long-ra-b";
        // Retry-After: 45 is beyond the kernel's 2s fast-path window and the
        // kernel never sleeps, so the chain must not start even though this pool
        // HAS a second eligible key: one attempt, the buffered rejection served
        // byte-for-byte including its Retry-After header, and an unmarked row.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                429,
                Some("45"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-long-ra"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry long retry-after" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("45"),
            "the upstream Retry-After must reach the client unchanged"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes());

        // The pool has an eligible second key, so ONLY the Retry-After gate can
        // explain the single attempt.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "a long Retry-After must not start a chain, eligible pool or not: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(rows[0].status_code, Some(429));
        assert!(
            rows[0].fallback_reason.is_none(),
            "an ineligible outcome carries no retry-chain marker, got {:?}",
            rows[0].fallback_reason
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_rotates_on_432_exhaustion() {
        let db_path = temp_db_path("http-search-cross-key-retry-432");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-432-a";
        let retry_key = "tvly-cross-key-retry-432-b";
        // 432 is Tavily's monthly run-out: the exhaustion isomorph of the 429
        // case, and an explicit rejection of the KEY rather than of the request.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                432,
                None,
                CROSS_KEY_RETRY_EXHAUSTED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-432"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry exhaustion" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes(),
            "the client must see the retry, never the buffered 432"
        );

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(keys.len(), 2, "one retry, never more: {seen:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].status_code, Some(432), "first hit row keeps the 432");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the exhausted first hit is marked attempted"
        );
        assert_eq!(rows[1].status_code, Some(200));
        assert_eq!(rows[1].result_status, "success");
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "the winning retry is marked succeeded"
        );

        // The losing key is marked exhausted, which is also why the strict
        // acquire would skip it even without the exclusion set.
        let losing_key_id = rows[0].api_key_id.clone().expect("first attempt names its key");
        assert_eq!(
            fetch_api_key_status(&pool, &losing_key_id).await,
            "exhausted",
            "432 marks the losing key exhausted"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].business_credits, Some(1));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_rotates_on_upstream_500() {
        let db_path = temp_db_path("http-search-cross-key-retry-500");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-500-a";
        let retry_key = "tvly-cross-key-retry-500-b";
        // The amendment's second arm: an upstream 5xx is not caused by the
        // request, so one bounded try on another key beats surfacing it.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                500,
                None,
                CROSS_KEY_RETRY_SERVER_ERROR_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-500"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry transient" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes(),
            "the client must see the retry, never the buffered 500"
        );

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        let keys = cross_key_retry_seen_keys(&seen);
        assert_eq!(keys.len(), 2, "one retry, never more: {seen:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].status_code, Some(500), "first hit row keeps the 500");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the transient first hit is marked attempted"
        );
        assert_eq!(rows[1].status_code, Some(200));
        assert_eq!(rows[1].result_status, "success");
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "the winning retry is marked succeeded"
        );

        // A 5xx marks nothing and arms no cooldown, so the rotation above can
        // only have come from the exclusion set: the key this attempt failed on
        // is never handed back, cooling or not.
        let losing_key_id = rows[0].api_key_id.clone().expect("first attempt names its key");
        assert_eq!(
            rows[0].key_effect_code, "none",
            "a transient 5xx must not mark the key"
        );
        assert!(
            fetch_armed_transient_backoffs(&pool, &losing_key_id)
                .await
                .is_empty(),
            "a transient 5xx must not cool the key either"
        );
        assert_ne!(
            losing_key_id,
            rows[1].api_key_id.clone().expect("retry names its key"),
            "the retry must not reuse the excluded key"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].business_credits, Some(1));

        let _ = std::fs::remove_file(db_path);
    }

    /// The amendment's second arm on an attempt that produced no usable upstream
    /// response: the first connection dies below HTTP, the seam rotates ONCE to
    /// another key and the client only ever sees the retry's success. See
    /// [`spawn_http_search_mock_failing_first_attempt_transport`] for why a
    /// wall-clock timeout is not injectable and for the two row shapes a dead
    /// first attempt may take (direct transport error vs. a proxy-answered 502).
    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_rotates_on_transport_failure() {
        let db_path = temp_db_path("http-search-cross-key-retry-transport");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-transport-a";
        let retry_key = "tvly-cross-key-retry-transport-b";
        let (upstream_addr, attempts, transport_failures) =
            spawn_http_search_mock_failing_first_attempt_transport().await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-transport"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "cross key retry transport" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        // The transport failure is absorbed: the client sees the retry's success.
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes());

        assert_eq!(
            transport_failures.load(Ordering::SeqCst),
            1,
            "exactly one attempt may die on the transport layer"
        );
        // The poisoned connection never produced an HTTP attempt, so the recorder
        // holds the retry only - one served attempt, one retry, never more.
        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "only the retry reaches upstream as an HTTP attempt: {seen:?}"
        );
        assert_eq!(seen[0].path, "/search");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            2,
            "the dead first attempt keeps its own visible row: {rows:?}"
        );
        // The dead first attempt is visible as its own row and is never a
        // success. Its status is the ONE environment-dependent detail: a direct
        // connection surfaces the transport error (no status at all), while a
        // transparent HTTP proxy in front of the mock answers 502 for the poisoned
        // upstream connection. Both are the transient arm, so both must rotate.
        assert!(
            rows[0].status_code.is_none()
                || matches!(rows[0].status_code, Some(500..=599)),
            "a dead first attempt is either a statusless transport error or a 5xx, got {:?}",
            rows[0].status_code
        );
        // `std::println!` on purpose: `runtime_logging` shadows the bare macro
        // crate-wide, so only this form reaches a `--nocapture` QA transcript.
        std::println!(
            "[task-7][manual-qa] dead first attempt shape: status={:?} poisoned_connections={} served_attempts={}",
            rows[0].status_code,
            transport_failures.load(Ordering::SeqCst),
            seen.len(),
        );
        assert_eq!(rows[0].result_status, "error");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the dead first attempt is marked attempted"
        );
        assert_eq!(
            rows[0].key_effect_code, "none",
            "a dead first attempt must not mark the key"
        );
        assert_eq!(rows[1].status_code, Some(200));
        assert_eq!(rows[1].result_status, "success");
        assert_eq!(
            rows[1].fallback_reason.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "the winning retry is marked succeeded"
        );
        assert_ne!(
            rows[0].api_key_id, rows[1].api_key_id,
            "the retry must rotate to another key: {rows:?}"
        );
        assert_eq!(
            rows[1]
                .api_key_id
                .clone()
                .expect("the retry names its key"),
            fetch_api_key_id_by_secret(&pool, &seen[0].api_key).await,
            "the winning row names the key that actually served the retry"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].business_credits, Some(1));

        let _ = std::fs::remove_file(db_path);
    }

    /// Shared pin for the amendment's request-shaped 4xx boundary: `status` must
    /// reach the client unchanged on a SINGLE upstream attempt, with the key
    /// neither marked nor cooled and no retry-chain marker - even though the pool
    /// holds a second eligible key and the mock would have served that retry
    /// successfully. Rotating here would burn the pool for a request the upstream
    /// blamed on the caller.
    async fn assert_request_shaped_client_error_is_served_as_is(
        status: u16,
        db_prefix: &str,
        token_note: &str,
    ) {
        let db_path = temp_db_path(db_prefix);
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = format!("tvly-{db_prefix}-a");
        let retry_key = format!("tvly-{db_prefix}-b");
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/search"),
                status,
                None,
                CROSS_KEY_RETRY_REQUEST_ERROR_BODY,
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
            .create_access_token(Some(token_note))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "request shaped client error" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status().as_u16(), status);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_REQUEST_ERROR_BODY.as_bytes(),
            "the original {status} body must reach the client unchanged"
        );

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "a request-shaped {status} must never rotate to another key: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(rows[0].status_code, Some(i64::from(status)));
        assert_ne!(
            rows[0].result_status, "success",
            "a request-shaped {status} is not a success"
        );
        assert!(
            rows[0].fallback_reason.is_none(),
            "an excluded request error carries no retry-chain marker, got {:?}",
            rows[0].fallback_reason
        );
        assert_eq!(
            rows[0].key_effect_code, "none",
            "a request-shaped {status} must not mark the key"
        );

        let used_key_id = rows[0]
            .api_key_id
            .clone()
            .expect("the attempt names its key");
        assert!(
            fetch_armed_transient_backoffs(&pool, &used_key_id)
                .await
                .is_empty(),
            "a request-shaped {status} must not cool the key it used"
        );
        assert_eq!(fetch_api_key_status(&pool, &used_key_id).await, "active");
        // The untouched second key stays selectable too: a request error burns no
        // part of the pool.
        let other_secret = if fetch_api_key_id_by_secret(&pool, &first_key).await == used_key_id {
            retry_key.clone()
        } else {
            first_key.clone()
        };
        let other_key_id = fetch_api_key_id_by_secret(&pool, &other_secret).await;
        assert_eq!(fetch_api_key_status(&pool, &other_key_id).await, "active");
        assert!(
            fetch_armed_transient_backoffs(&pool, &other_key_id)
                .await
                .is_empty(),
            "the key that was never tried must stay untouched"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(i64::from(status)));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_request_shaped_400_is_served_as_is_without_rotating() {
        assert_request_shaped_client_error_is_served_as_is(
            400,
            "http-search-request-shaped-400",
            "http-search-request-shaped-400",
        )
        .await;
    }

    #[tokio::test]
    async fn tavily_http_search_request_shaped_422_is_served_as_is_without_rotating() {
        assert_request_shaped_client_error_is_served_as_is(
            422,
            "http-search-request-shaped-422",
            "http-search-request-shaped-422",
        )
        .await;
    }

    #[tokio::test]
    async fn tavily_http_research_post_429_is_never_replayed_on_another_key() {
        let db_path = temp_db_path("http-research-cross-key-retry-excluded");
        let db_str = db_path.to_string_lossy().to_string();

        // Avoid cross-test env var interference (research enforces a model estimate).
        let _hourly_business_guard = EnvVarGuard::set("TOKEN_HOURLY_LIMIT", "1000");

        let first_key = "tvly-research-excluded-a";
        let retry_key = "tvly-research-excluded-b";
        // A research task may already be running upstream, so it is NEVER
        // replayed: the pool holds a second eligible key, the Retry-After sits
        // inside the fast-path window and the mock would serve the retry, yet the
        // surface exclusion must beat all of that.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/research"),
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-research-cross-key-retry-excluded"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/research"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({
                "input": "cross key retry exclusion",
                "model": "mini"
            }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the upstream Retry-After header must be preserved"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes());

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            cross_key_retry_attempts_on(&seen, "/research").len(),
            1,
            "/research is never replayed on another key: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        let research_rows: Vec<&CrossKeyRetryLogRow> = rows
            .iter()
            .filter(|row| row.path == "/api/tavily/research")
            .collect();
        assert_eq!(
            research_rows.len(),
            1,
            "one attempt, one visible row: {rows:?}"
        );
        assert_eq!(research_rows[0].status_code, Some(429));
        assert!(
            research_rows[0].fallback_reason.is_none(),
            "an excluded surface carries no retry-chain marker, got {:?}",
            research_rows[0].fallback_reason
        );

        // The reservation taken for the estimate is released, not doubled: one
        // token log for the whole logical request.
        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(429));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_research_result_get_429_is_never_replayed_on_another_key() {
        let db_path = temp_db_path("http-research-result-cross-key-retry-excluded");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-research-result-excluded-a";
        let retry_key = "tvly-research-result-excluded-b";
        let request_id = "req-cross-key-retry-excluded";

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-research-result-cross-key-retry-excluded"))
            .await
            .expect("create token");

        // The result poll is pinned to the key that owns the research task, so
        // seed that ownership: without it the handler answers 404 before any
        // upstream call and the exclusion would never be exercised.
        let pool = connect_sqlite_test_pool(&db_str).await;
        let pinned_key_id = fetch_api_key_id_by_secret(&pool, first_key).await;
        let now = Utc::now().timestamp();
        sqlx::query(
            r#"
            INSERT INTO research_requests (request_id, key_id, token_id, expires_at, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(request_id)
        .bind(&pinned_key_id)
        .bind(&token.id)
        .bind(now + 3600)
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .expect("seed research request owner");

        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::ResearchResultGet,
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .get(format!(
                "http://{proxy_addr}/api/tavily/research/{request_id}"
            ))
            .header("Authorization", format!("Bearer {}", token.token))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(
            body.as_ref(),
            CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes(),
            "the result poll must pass the upstream rejection through"
        );

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(seen.len(), 1, "the result poll is never replayed: {seen:?}");
        assert_eq!(seen[0].path, format!("/research/{request_id}"));

        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(rows[0].path, format!("/api/tavily/research/{request_id}"));
        assert_eq!(rows[0].status_code, Some(429));
        assert!(
            rows[0].fallback_reason.is_none(),
            "an excluded surface carries no retry-chain marker, got {:?}",
            rows[0].fallback_reason
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_crawl_429_is_never_replayed_on_another_key() {
        let db_path = temp_db_path("http-crawl-cross-key-retry-excluded");
        let db_str = db_path.to_string_lossy().to_string();

        // Avoid cross-test env var interference (crawl reserves expected credits).
        let _hourly_business_guard = EnvVarGuard::set("TOKEN_HOURLY_LIMIT", "1000");

        let first_key = "tvly-crawl-excluded-a";
        let retry_key = "tvly-crawl-excluded-b";
        // A crawl job has side effects upstream, so it is never replayed even
        // though the pool holds a willing second key.
        let (upstream_addr, attempts) = spawn_http_cross_key_retry_mock(
            CrossKeyRetryMockPlan::rotating_after_rejection(
                CrossKeyRetryRejectedRoute::JsonPost("/crawl"),
                429,
                Some("0"),
                CROSS_KEY_RETRY_RATE_LIMITED_BODY,
            ),
        )
        .await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-crawl-cross-key-retry-excluded"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/crawl"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "urls": ["https://example.com/page"] }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the upstream Retry-After header must be preserved"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_RATE_LIMITED_BODY.as_bytes());

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            cross_key_retry_attempts_on(&seen, "/crawl").len(),
            1,
            "/crawl is never replayed on another key: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        let crawl_rows: Vec<&CrossKeyRetryLogRow> = rows
            .iter()
            .filter(|row| row.path == "/api/tavily/crawl")
            .collect();
        assert_eq!(crawl_rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(crawl_rows[0].status_code, Some(429));
        assert!(
            crawl_rows[0].fallback_reason.is_none(),
            "an excluded surface carries no retry-chain marker, got {:?}",
            crawl_rows[0].fallback_reason
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(429));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_success_never_enters_the_cross_key_retry_chain() {
        let db_path = temp_db_path("http-search-cross-key-retry-success");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-cross-key-retry-success-a";
        let retry_key = "tvly-cross-key-retry-success-b";
        // Regression pin for the other side of the seam: a plain success must not
        // be retried, re-marked or re-billed, and must not grow a marker.
        let (upstream_addr, attempts) =
            spawn_http_cross_key_retry_mock(CrossKeyRetryMockPlan::all_success()).await;
        let usage_base = format!("http://{upstream_addr}");

        let proxy = TavilyProxy::with_endpoint(
            vec![first_key.to_string(), retry_key.to_string()],
            DEFAULT_UPSTREAM,
            &db_str,
        )
        .await
        .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-cross-key-retry-success"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "plain success" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_SECOND_HIT_BODY.as_bytes());

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(seen.len(), 1, "a success is one upstream hit: {seen:?}");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(rows[0].status_code, Some(200));
        assert_eq!(rows[0].result_status, "success");
        assert!(
            rows[0].fallback_reason.is_none(),
            "a success carries no retry-chain marker, got {:?}",
            rows[0].fallback_reason
        );
        assert_eq!(
            rows[0].key_effect_code, "none",
            "a success on an active key marks nothing"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].business_credits, Some(1));

        let _ = std::fs::remove_file(db_path);
    }
