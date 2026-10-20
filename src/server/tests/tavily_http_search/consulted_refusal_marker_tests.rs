    // ---------------------------------------------------------------------
    // Consulted-refusal marker semantics on the LEGACY Tavily HTTP executor
    // (plan prod-request-errors todo 11; the residual named by todo 5).
    //
    // Production row id 1798 (tavily 432, result_status quota_exhausted,
    // fallback_reason NULL) most plausibly hit the seam's early ServeOriginal
    // return: the kernel was consulted, refused the replay (a Retry-After
    // beyond the fast path or a spent deadline), and the executor dropped the
    // marker to None. Todo 5 fixed the provider-facade wrapper; this fragment
    // pins the same contract on the legacy executor:
    // * a consulted refusal that serves the first hit unchanged is `rejected`;
    // * a consulted refusal whose strict acquire finds no key is
    //   `skipped_no_eligible_key`;
    // * a plain success stays unmarked (no retry decision exists).
    //
    // This fragment is `include!`d from `tavily_http_search.rs` for the same
    // line-budget reason as its siblings. include! keeps the module path and
    // the registered CI manifest prefix `server::tests::tavily_http_search::`;
    // rustfmt never visits an include!-only file, so the four-space module
    // indentation is kept by hand.

    #[tokio::test]
    async fn tavily_http_search_consulted_retry_after_refusal_is_marked_rejected() {
        let db_path = temp_db_path("http-search-consulted-refusal-long-ra");
        let db_str = db_path.to_string_lossy().to_string();

        let first_key = "tvly-consulted-refusal-long-ra-a";
        let retry_key = "tvly-consulted-refusal-long-ra-b";
        // Retry-After: 45 is beyond the kernel's 2s fast-path window and the
        // kernel never sleeps. The pool HAS a second eligible key and the mock
        // would serve it, so only the consulted refusal explains the single
        // attempt - and that refusal must be visible as `rejected`, never NULL.
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
            .create_access_token(Some("http-search-consulted-refusal-long-ra"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "consulted refusal" }))
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
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:rejected"),
            "the consulted refusal is marked rejected, never NULL: {rows:?}"
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
    async fn tavily_http_search_consulted_refusal_without_eligible_key_is_marked_skipped() {
        let db_path = temp_db_path("http-search-consulted-refusal-no-eligible");
        let db_str = db_path.to_string_lossy().to_string();

        let only_key = "tvly-consulted-refusal-only";
        // A 432 is the production row-1798 shape (tavily quota_exhausted): the
        // kernel is consulted and the strict acquire then finds no other key.
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

        let proxy =
            TavilyProxy::with_endpoint(vec![only_key.to_string()], DEFAULT_UPSTREAM, &db_str)
                .await
                .expect("proxy created");
        let token = proxy
            .create_access_token(Some("http-search-consulted-refusal-no-eligible"))
            .await
            .expect("create token");
        let proxy_addr = spawn_proxy_server(proxy, usage_base).await;

        let resp = Client::new()
            .post(format!("http://{proxy_addr}/api/tavily/search"))
            .header("Authorization", format!("Bearer {}", token.token))
            .json(&serde_json::json!({ "query": "consulted refusal empty pool" }))
            .send()
            .await
            .expect("request to proxy succeeds");

        assert_eq!(
            resp.status().as_u16(),
            432,
            "the buffered first hit is served unchanged"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), CROSS_KEY_RETRY_EXHAUSTED_BODY.as_bytes());

        let seen = attempts.lock().expect("attempts lock poisoned").clone();
        assert_eq!(
            seen.len(),
            1,
            "an empty retry pool must not start a chain: {seen:?}"
        );
        assert_eq!(seen[0].api_key, only_key);

        let pool = connect_sqlite_test_pool(&db_str).await;
        let rows = fetch_cross_key_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].status_code, Some(432));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "a consulted refusal with an empty pool is marked skipped: {rows:?}"
        );

        let token_logs = fetch_cross_key_retry_token_logs(&pool, &token.id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(432));

        let _ = std::fs::remove_file(db_path);
    }
