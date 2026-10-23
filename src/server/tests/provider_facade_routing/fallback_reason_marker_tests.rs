// ---------------------------------------------------------------------
    // fallback_reason marker semantics (plan prod-request-errors todo 5).
    //
    // Production evidence: post-release quota_exhausted rows carried
    // `fallback_reason = NULL` (9 x SerpApi run-out 429, 1 x Tavily 432, 1 x
    // Exa MCP 402). The wrapper used to map every consulted decision that did
    // not rotate back to `NotInChain` -> None, so an operator could not tell a
    // consulted refusal from an attempt the kernel never saw.
    //
    // The battery below pins the corrected contract:
    // * a consulted retry that DOES run leaves attempted -> succeeded/rejected;
    // * a consulted retry whose strict acquire finds no key leaves
    //   skipped_no_eligible_key;
    // * a consulted refusal (Retry-After beyond the fast path, veto,
    //   request-shaped 4xx) leaves rejected - never NULL;
    // * the transport arm (no HTTP response at all) funnels through the same
    //   marker mapping and marks its audit row too;
    // * the Tavily HTTP isomorph (432 exhaustion) keeps its chain markers
    //   visible on the same test surface.
    //
    // This fragment is `include!`d from `provider_facade_routing.rs` because
    // the module already sits close to the `rust_source_line_budgets` ceiling.
    // `include!` keeps the module path `server::tests::provider_facade_routing`
    // (and therefore the registered manifest prefix) unchanged. rustfmt never
    // visits an include!-only file, so the four-space module indentation below
    // is kept by hand.

    impl FacadeRetryPoolKeys {
        /// Exa pool narrowed to ONE key: a key-scoped 402 rejects the whole
        /// pool, so the strict acquire has nothing left to offer.
        fn single_exa_key() -> Self {
            Self {
                exa: vec!["mock-exa-key-only".to_string()],
                ..Self::two_per_provider()
            }
        }

        /// Brave pool narrowed to ONE key: the poisoned first connection is the
        /// whole pool, so the transport arm cannot rotate.
        fn single_brave_key() -> Self {
            Self {
                brave: vec!["mock-brave-key-only".to_string()],
                ..Self::two_per_provider()
            }
        }
    }

    #[tokio::test]
    async fn serpapi_facade_run_out_429_without_a_second_key_marks_skipped_no_eligible_key() {
        // SerpApi's run-out 429 is an EXHAUSTION rejection, so the kernel is
        // consulted and answers Retry. The single-key pool then makes the
        // strict acquire come back empty: the row must carry
        // skipped_no_eligible_key, not NULL.
        let harness = spawn_provider_facade_retry_harness_with_pool(
            "provider-facade-marker-serpapi-run-out-no-eligible",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                429,
                None,
                FACADE_RETRY_SERPAPI_RUN_OUT_BODY,
            ),
            FacadeRetryPoolKeys::single_serpapi_key(),
        )
        .await;

        let resp = Client::new()
            .get(format!(
                "http://{}/api/serpapi/search?q=rust&engine=google",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("serpapi request");

        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_SERPAPI_RUN_OUT_BODY.as_bytes());

        let seen = facade_retry_seen(&harness);
        assert_eq!(seen.len(), 1, "single attempt only: {seen:?}");
        assert_eq!(seen[0].api_key, "mock-serpapi-key-only");

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("serpapi"));
        assert_eq!(rows[0].status_code, Some(429));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "a consulted decision with an empty retry pool is marked, never NULL: {rows:?}"
        );
        assert_eq!(rows[0].key_effect_code, "marked_exhausted");
        pool.close().await;
    }

    #[tokio::test]
    async fn serpapi_facade_run_out_429_with_a_long_retry_after_marks_the_consulted_rejection() {
        // The kernel never sleeps: a Retry-After beyond the 2s fast-path window
        // makes it serve the first hit unchanged. That is a CONSULTED refusal,
        // so the row must carry `rejected`; the pre-fix wrapper returned
        // NotInChain here and the production row kept fallback_reason NULL.
        let harness = spawn_provider_facade_retry_harness(
            "provider-facade-marker-serpapi-long-retry-after",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                429,
                Some("30"),
                FACADE_RETRY_SERPAPI_RUN_OUT_BODY,
            ),
        )
        .await;

        let resp = Client::new()
            .get(format!(
                "http://{}/api/serpapi/search?q=rust&engine=google",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("serpapi request");

        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("30"),
            "the long upstream Retry-After must survive the passthrough"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_SERPAPI_RUN_OUT_BODY.as_bytes());

        let seen = facade_retry_seen(&harness);
        assert_eq!(
            seen.len(),
            1,
            "a Retry-After beyond the fast path never replays: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the consulted attempt stays visible: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("serpapi"));
        assert_eq!(rows[0].status_code, Some(429));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:rejected"),
            "a consulted refusal is marked rejected, never NULL: {rows:?}"
        );
        assert_eq!(
            rows[0].key_effect_code, "marked_exhausted",
            "skipping the retry does not skip key marking"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn exa_facade_key_scoped_402_without_a_second_key_marks_skipped_no_eligible_key() {
        // NO_MORE_CREDITS is KEY-scoped, so the veto does not fire and the
        // kernel is consulted. With one Exa key the strict acquire finds
        // nothing: the quota_exhausted row must be marked skipped, not NULL.
        let harness = spawn_provider_facade_retry_harness_with_pool(
            "provider-facade-marker-exa-key-402-no-eligible",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                402,
                None,
                FACADE_RETRY_EXA_KEY_BUDGET_BODY,
            ),
            FacadeRetryPoolKeys::single_exa_key(),
        )
        .await;

        let resp = Client::new()
            .post(format!("http://{}/api/exa/search", harness.addr))
            .header("x-api-key", harness.token.clone())
            .header("content-type", "application/json")
            .body(r#"{"query":"rust"}"#)
            .send()
            .await
            .expect("exa request");

        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_EXA_KEY_BUDGET_BODY.as_bytes());

        let seen = facade_retry_seen(&harness);
        assert_eq!(seen.len(), 1, "single attempt only: {seen:?}");
        assert_eq!(seen[0].api_key, "mock-exa-key-only");

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("exa"));
        assert_eq!(rows[0].status_code, Some(402));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "the key-scoped 402 consulted the kernel and found no retry key: {rows:?}"
        );
        assert_eq!(rows[0].key_effect_code, "marked_exhausted");
        pool.close().await;
    }

    #[tokio::test]
    async fn brave_facade_transport_failure_without_a_second_key_marks_skipped_no_eligible_key() {
        // The transport arm has no HTTP response, so its audit row is written
        // by the Err branch. The kernel is still consulted on the transport
        // evidence; with the only Brave key excluded there is nowhere to replay,
        // and the row must carry skipped_no_eligible_key (the wrapper tail that
        // used to return NotInChain -> None).
        let harness = spawn_provider_facade_retry_harness_with_pool(
            "provider-facade-marker-brave-transport-no-eligible",
            FacadeRetryMockPlan::first_connection_dies_below_http("/res/v1/web/search"),
            FacadeRetryPoolKeys::single_brave_key(),
        )
        .await;

        let resp = Client::new()
            .get(format!(
                "http://{}/api/brave/res/v1/web/search?q=rust",
                harness.addr
            ))
            .header("x-subscription-token", harness.token.clone())
            .send()
            .await
            .expect("brave request");

        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an unretryable transport failure still surfaces through the 502 boundary"
        );
        assert_eq!(
            harness.transport_failures.load(Ordering::SeqCst),
            1,
            "exactly one attempt may die below HTTP"
        );
        let seen = facade_retry_seen(&harness);
        assert!(
            seen.is_empty(),
            "the poisoned connection never produced an HTTP attempt: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the dead attempt keeps its audit row: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("brave"));
        assert!(
            rows[0].status_code.is_none() || matches!(rows[0].status_code, Some(500..=599)),
            "a dead attempt is statusless or proxied as 5xx, got {:?}",
            rows[0].status_code
        );
        assert_eq!(rows[0].result_status, "error");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "the transport arm consults the kernel and marks its empty pool: {rows:?}"
        );
        assert_eq!(rows[0].key_effect_code, "none");
        pool.close().await;
    }

    // ---------------------------------------------------------------------
    // Tavily HTTP 432 exhaustion (the production row that is NOT a provider
    // REST facade): same marker vocabulary through the legacy HTTP executor.
    // The mock injects the pooled key into the JSON body exactly as the
    // executor does, so the two seeded keys are the only ones that can reach
    // upstream.
    // ---------------------------------------------------------------------

    const TAVILY_432_BODY: &str = r#"{"error":"monthly quota exhausted"}"#;
    const TAVILY_432_SECOND_HIT_BODY: &str =
        r#"{"status":200,"results":[{"title":"second key hit"}],"usage":{"credits":1}}"#;
    const TAVILY_432_USAGE_BODY: &str = r#"{"key":{"research_usage":10}}"#;

    /// One Tavily HTTP upstream attempt: the pooled key carried in the injected
    /// `api_key` body field.
    type Tavily432Attempts = Arc<Mutex<Vec<String>>>;

    struct Tavily432Harness {
        addr: SocketAddr,
        attempts: Tavily432Attempts,
        token: String,
        token_id: String,
        db_str: String,
        db_path: PathBuf,
    }

    impl Drop for Tavily432Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.db_path);
        }
    }

    /// Mock Tavily HTTP upstream: the first `/search` attempt answers 432, every
    /// later one succeeds. `/usage` answers a fixed probe body so an incidental
    /// usage read never disturbs the attempt counter.
    fn tavily_432_mock_router(attempts: Tavily432Attempts) -> Router {
        let search = move |Json(body): Json<Value>| {
            let attempts = attempts.clone();
            async move {
                let api_key = body
                    .get("api_key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !api_key.is_empty(),
                    "upstream /search must receive an injected Tavily key"
                );
                let attempt = {
                    let mut seen = attempts.lock().expect("attempts lock poisoned");
                    seen.push(api_key);
                    seen.len()
                };
                let (status, payload) = if attempt == 1 {
                    (432u16, TAVILY_432_BODY)
                } else {
                    (200u16, TAVILY_432_SECOND_HIT_BODY)
                };
                Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(payload))
                    .expect("build tavily 432 mock response")
            }
        };
        let usage = || async {
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(TAVILY_432_USAGE_BODY))
                .expect("build tavily usage response")
        };
        Router::new()
            .route("/search", post(search))
            .route("/usage", get(usage))
    }

    async fn spawn_tavily_432_harness(prefix: &str, keys: Vec<&str>) -> Tavily432Harness {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let attempts: Tavily432Attempts = Arc::new(Mutex::new(Vec::new()));
        let app = tavily_432_mock_router(attempts.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });
        let upstream = format!("http://{upstream_addr}");
        let pool_keys: Vec<String> = keys.into_iter().map(str::to_string).collect();
        let proxy = TavilyProxy::with_endpoint(pool_keys, DEFAULT_UPSTREAM, &db_str)
            .await
            .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some(prefix))
            .await
            .expect("create access token");
        let addr = spawn_proxy_server(proxy, upstream).await;
        Tavily432Harness {
            addr,
            attempts,
            token: access_token.token,
            token_id: access_token.id,
            db_str,
            db_path,
        }
    }

    #[tokio::test]
    async fn tavily_http_432_rotates_to_a_second_key_and_marks_attempted_then_succeeded() {
        let harness = spawn_tavily_432_harness(
            "provider-facade-marker-tavily-432-rotate",
            vec!["tvly-facade-marker-a", "tvly-facade-marker-b"],
        )
        .await;

        let resp = Client::new()
            .post(format!("http://{}/api/tavily/search", harness.addr))
            .header("Authorization", format!("Bearer {}", harness.token))
            .json(&serde_json::json!({ "query": "marker semantics" }))
            .send()
            .await
            .expect("tavily request");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), TAVILY_432_SECOND_HIT_BODY.as_bytes());

        let seen = harness
            .attempts
            .lock()
            .expect("attempts lock poisoned")
            .clone();
        assert_eq!(seen.len(), 2, "one retry, never more: {seen:?}");
        assert_ne!(seen[0], seen[1], "the retry must rotate to another key");
        assert!(
            seen.iter()
                .all(|key| key == "tvly-facade-marker-a" || key == "tvly-facade-marker-b"),
            "only seeded pool keys may reach upstream: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("tavily"));
        assert_eq!(rows[1].provider.as_deref(), Some("tavily"));
        assert_eq!(rows[0].status_code, Some(432));
        assert_eq!(rows[0].result_status, "quota_exhausted");
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
        assert_ne!(
            rows[0].api_key_id, rows[1].api_key_id,
            "the two rows must name two different pool keys: {rows:?}"
        );

        let token_logs = fetch_facade_retry_token_logs(&pool, &harness.token_id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].http_status, Some(200));
        pool.close().await;
    }

    #[tokio::test]
    async fn tavily_http_432_without_a_second_key_marks_skipped_no_eligible_key() {
        let harness = spawn_tavily_432_harness(
            "provider-facade-marker-tavily-432-no-eligible",
            vec!["tvly-facade-marker-only"],
        )
        .await;

        let resp = Client::new()
            .post(format!("http://{}/api/tavily/search", harness.addr))
            .header("Authorization", format!("Bearer {}", harness.token))
            .json(&serde_json::json!({ "query": "marker semantics" }))
            .send()
            .await
            .expect("tavily request");

        assert_eq!(
            resp.status().as_u16(),
            432,
            "the exhausted first hit is served unchanged"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), TAVILY_432_BODY.as_bytes());

        let seen = harness
            .attempts
            .lock()
            .expect("attempts lock poisoned")
            .clone();
        assert_eq!(seen.len(), 1, "single attempt only: {seen:?}");
        assert_eq!(seen[0], "tvly-facade-marker-only");

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].status_code, Some(432));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert_eq!(
            rows[0].fallback_reason.as_deref(),
            Some("upstream_key_retry:skipped_no_eligible_key"),
            "a consulted Tavily 432 with an empty retry pool is marked: {rows:?}"
        );
        assert_eq!(rows[0].key_effect_code, "marked_exhausted");

        let token_logs = fetch_facade_retry_token_logs(&pool, &harness.token_id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(432));
        pool.close().await;
    }

    #[tokio::test]
    async fn brave_facade_with_every_proxy_node_exiled_returns_502_and_names_the_egress_gap() {
        // 2026-10-22 Brave audit, operator-facing contract: when the LAN cannot
        // reach the provider directly and every forward-proxy node is exiled,
        // the request must (a) never be rerouted to the blocked direct egress,
        // (b) surface as 502, and (c) leave a row whose failure_kind NAMES the
        // proxy gap. Before the egress policy, this same state produced a
        // transport_send_error row that read as a network fault.
        let harness = spawn_provider_facade_retry_harness_with_pool(
            "provider-facade-egress-brave-all-nodes-exiled",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/res/v1/web/search",
                200,
                None,
                "{}",
            ),
            FacadeRetryPoolKeys::single_brave_key(),
        )
        .await;

        let proxy_url = "http://127.0.0.1:9".to_string();
        harness
            .proxy
            .update_forward_proxy_settings(
                ForwardProxySettings {
                    proxy_urls: vec![proxy_url.clone()],
                    subscription_urls: Vec::new(),
                    subscription_update_interval_secs: 3600,
                    // No direct candidate may even be planned: that is the
                    // deployment shape the policy exists for.
                    insert_direct: false,
                    egress_socks5_enabled: false,
                    egress_socks5_url: String::new(),
                },
                true,
            )
            .await
            .expect("configure the forward-proxy pool");
        harness
            .proxy
            .set_forward_proxy_nodes_disabled(vec![proxy_url], true)
            .await
            .expect("exile the only proxy node");

        let resp = Client::new()
            .get(format!(
                "http://{}/api/brave/res/v1/web/search?q=rust",
                harness.addr
            ))
            .header("x-subscription-token", harness.token.clone())
            .send()
            .await
            .expect("brave request");

        assert_eq!(
            resp.status(),
            StatusCode::BAD_GATEWAY,
            "an exhausted proxy pool is a gateway failure, not a client error"
        );
        let seen = facade_retry_seen(&harness);
        assert!(
            seen.is_empty(),
            "nothing may reach the upstream while every proxy node is exiled: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(
            rows.len(),
            1,
            "the refused attempt still keeps exactly one audit row: {rows:?}"
        );
        assert_eq!(rows[0].provider.as_deref(), Some("brave"));
        assert_eq!(rows[0].result_status, "error");
        assert_eq!(
            rows[0].failure_kind.as_deref(),
            Some("forward_proxy_egress_unavailable"),
            "the row must name the proxy gap instead of a transport fault: {rows:?}"
        );
        assert!(
            rows[0].status_code.is_none(),
            "nothing was sent, so there is no upstream status: {rows:?}"
        );
        assert_eq!(rows[0].key_effect_code, "none");
        pool.close().await;
    }
