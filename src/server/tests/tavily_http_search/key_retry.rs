    // Bounded cross-key retry seam coverage (plan provider-key-429-retry todo 6).
    //
    // This fragment is `include!`d from `tavily_http_search.rs` because that file
    // already sits at its `rust_source_line_budgets` ceiling (3213/3250) and the
    // plan forbids raising a budget. `include!` keeps the module path - and with
    // it the CI manifest prefix `server::tests::tavily_http_search::tavily_` -
    // unchanged, so no manifest edit is needed either.

    /// Body the mock returns when it rejects the first attempt.
    const CROSS_KEY_RETRY_RATE_LIMITED_BODY: &str = r#"{"error":"rate limited"}"#;
    /// Body the mock returns on every attempt after the first one. Distinct from
    /// the rejection body so the client-visible bytes prove WHICH attempt was
    /// served.
    const CROSS_KEY_RETRY_SECOND_HIT_BODY: &str =
        r#"{"status":200,"results":[{"title":"second key hit"}],"usage":{"credits":1}}"#;

    /// Mock Tavily upstream for the cross-key retry seam. Every attempt answers
    /// 429 with the given `retry_after` header - `"0"` sits inside the kernel's
    /// 2s fast-path window so a retry is immediate and never sleeps, a longer
    /// value is refused - except that with `only_first_hit_rejected` every
    /// attempt after the first succeeds instead. Each attempt's body `api_key`
    /// and bearer token are recorded in order, which is how the tests prove the
    /// retry rotated to a different key and that the chain stopped where the
    /// budget says it must.
    async fn spawn_http_search_mock_rate_limited(
        only_first_hit_rejected: bool,
        retry_after: &'static str,
    ) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let seen_keys = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = Router::new().route(
            "/search",
            post({
                let seen_keys = seen_keys.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let seen_keys = seen_keys.clone();
                    async move {
                        let api_key = body
                            .get("api_key")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let bearer = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.strip_prefix("Bearer "))
                            .unwrap_or_default()
                            .to_string();
                        assert_eq!(
                            api_key, bearer,
                            "body api_key and upstream bearer must carry the same key"
                        );
                        assert!(
                            !api_key.is_empty(),
                            "upstream /search must receive an injected Tavily key"
                        );
                        let attempt = {
                            let mut seen = seen_keys.lock().expect("seen keys lock poisoned");
                            seen.push(api_key);
                            seen.len()
                        };
                        if attempt == 1 || !only_first_hit_rejected {
                            Response::builder()
                                .status(StatusCode::TOO_MANY_REQUESTS)
                                .header(axum::http::header::CONTENT_TYPE, "application/json")
                                .header(axum::http::header::RETRY_AFTER, retry_after)
                                .body(Body::from(CROSS_KEY_RETRY_RATE_LIMITED_BODY))
                                .expect("build 429 response")
                        } else {
                            Response::builder()
                                .status(StatusCode::OK)
                                .header(axum::http::header::CONTENT_TYPE, "application/json")
                                .body(Body::from(CROSS_KEY_RETRY_SECOND_HIT_BODY))
                                .expect("build second-hit response")
                        }
                    }
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        (addr, seen_keys)
    }

    /// The retry chain's `request_logs` rows in attempt order:
    /// `(status_code, result_status, fallback_reason)`.
    async fn fetch_cross_key_retry_request_log_rows(
        pool: &sqlx::SqlitePool,
    ) -> Vec<(Option<i64>, String, Option<String>)> {
        let rows = sqlx::query(
            r#"
            SELECT status_code, result_status, fallback_reason
            FROM observability.request_logs
            ORDER BY id ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .expect("fetch request log rows");
        rows.iter()
            .map(|row| {
                (
                    row.try_get::<Option<i64>, _>("status_code").unwrap(),
                    row.try_get::<String, _>("result_status").unwrap(),
                    row.try_get::<Option<String>, _>("fallback_reason")
                        .unwrap(),
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
        let (upstream_addr, seen_keys) = spawn_http_search_mock_rate_limited(true, "0").await;
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
        let body = resp.text().await.expect("read response body");
        assert_eq!(body, CROSS_KEY_RETRY_SECOND_HIT_BODY);

        // Exactly one extra attempt, on a DIFFERENT key of the seeded pool.
        let keys = seen_keys.lock().expect("seen keys lock poisoned").clone();
        assert_eq!(keys.len(), 2, "one retry, never more: {keys:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");
        assert!(
            keys.iter()
                .all(|key| key == first_key || key == retry_key),
            "only seeded pool keys may reach upstream: {keys:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;

        // One visible request_logs row per attempt, marked attempted -> succeeded.
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].0, Some(429), "first hit row keeps the 429");
        assert_eq!(
            rows[0].2.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the losing first hit is marked attempted"
        );
        assert_eq!(rows[1].0, Some(200), "second hit row is the success");
        assert_eq!(rows[1].1, "success");
        assert_eq!(
            rows[1].2.as_deref(),
            Some("upstream_key_retry:succeeded"),
            "the winning retry is marked succeeded"
        );

        // Billing settles ONCE, on the final response: a single auth_token_logs
        // row charging the second hit's usage.
        let token_logs = sqlx::query(
            r#"
            SELECT result_status, business_credits
            FROM auth_token_logs
            WHERE token_id = ?
            ORDER BY id ASC
            "#,
        )
        .bind(&token.id)
        .fetch_all(&pool)
        .await
        .expect("fetch token log rows");
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt"
        );
        assert_eq!(
            token_logs[0]
                .try_get::<String, _>("result_status")
                .unwrap(),
            "success"
        );
        assert_eq!(
            token_logs[0]
                .try_get::<Option<i64>, _>("business_credits")
                .unwrap(),
            Some(1),
            "the retry is not billed twice"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn tavily_http_search_cross_key_retry_serves_first_hit_bytes_without_eligible_key() {
        let db_path = temp_db_path("http-search-cross-key-retry-empty-pool");
        let db_str = db_path.to_string_lossy().to_string();

        let only_key = "tvly-cross-key-retry-only";
        let (upstream_addr, seen_keys) = spawn_http_search_mock_rate_limited(true, "0").await;
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
        // upstream Retry-After header and the body all survive, and the 502 path
        // is never reused.
        assert_eq!(resp.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the upstream Retry-After header must be preserved"
        );
        let body = resp.text().await.expect("read response body");
        assert_eq!(body, CROSS_KEY_RETRY_RATE_LIMITED_BODY);

        // No second upstream call was ever made.
        let keys = seen_keys.lock().expect("seen keys lock poisoned").clone();
        assert_eq!(keys, vec![only_key.to_string()], "single attempt only");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].0, Some(429));
        assert_eq!(
            rows[0].2.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "an empty pool is marked, not silently retried"
        );

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
        let (upstream_addr, seen_keys) = spawn_http_search_mock_rate_limited(false, "0").await;
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
        let body = resp.text().await.expect("read response body");
        assert_eq!(body, CROSS_KEY_RETRY_RATE_LIMITED_BODY);

        // Hard cap: the first attempt plus ONE retry, on two different keys.
        let keys = seen_keys.lock().expect("seen keys lock poisoned").clone();
        assert_eq!(keys.len(), 2, "never a third upstream attempt: {keys:?}");
        assert_ne!(keys[0], keys[1], "the retry must rotate to another key");

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].0, Some(429));
        assert_eq!(
            rows[0].2.as_deref(),
            Some("upstream_key_retry:attempted"),
            "the losing first hit is marked attempted"
        );
        assert_eq!(rows[1].0, Some(429));
        assert_eq!(
            rows[1].2.as_deref(),
            Some("upstream_key_retry:rejected"),
            "a failed retry terminates the chain as rejected"
        );

        // One token log per logical request even though two attempts ran, and it
        // describes the FINAL attempt: the retry never re-enters dispatch,
        // reservation or settlement.
        let token_logs = sqlx::query(
            r#"
            SELECT result_status, http_status
            FROM auth_token_logs
            WHERE token_id = ?
            ORDER BY id ASC
            "#,
        )
        .bind(&token.id)
        .fetch_all(&pool)
        .await
        .expect("fetch token log rows");
        assert_eq!(
            token_logs.len(),
            1,
            "one token log per logical request, never one per attempt"
        );
        assert_eq!(
            token_logs[0]
                .try_get::<String, _>("result_status")
                .unwrap(),
            "error",
            "a rejected chain is logged as the failure it ended with"
        );
        assert_eq!(
            token_logs[0].try_get::<Option<i64>, _>("http_status").unwrap(),
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
        let (upstream_addr, seen_keys) = spawn_http_search_mock_rate_limited(true, "45").await;
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
        let body = resp.text().await.expect("read response body");
        assert_eq!(body, CROSS_KEY_RETRY_RATE_LIMITED_BODY);

        // The pool has an eligible second key, so ONLY the Retry-After gate can
        // explain the single attempt.
        let keys = seen_keys.lock().expect("seen keys lock poisoned").clone();
        assert_eq!(
            keys.len(),
            1,
            "a long Retry-After must not start a chain, eligible pool or not: {keys:?}"
        );

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "one attempt, one visible row: {rows:?}");
        assert_eq!(rows[0].0, Some(429));
        assert!(
            rows[0].2.is_none(),
            "an ineligible outcome carries no retry-chain marker, got {:?}",
            rows[0].2
        );

        let _ = std::fs::remove_file(db_path);
    }
