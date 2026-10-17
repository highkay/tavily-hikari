use super::*;
    use super::core_support_and_parsing::*;
    use tavily_hikari::providers::ProviderId;

    /// Plan todo 11 bin coverage: table-driven resolver, dual-slot client auth
    /// matrix (universal Bearer + provider-native slots), strip-before-forward
    /// (M17), M2 x-project-id exclusion, M16 provider MCP subpath rejection,
    /// and the uniform gate seams. Provider-specific execution bodies
    /// (billing settle, translation) are asserted only up to the documented
    /// todo 12-17 boundary.

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        method: String,
        path: String,
        query: Option<String>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }

        fn has_header(&self, name: &str) -> bool {
            self.header(name).is_some()
        }
    }

    async fn spawn_capturing_mock_upstream() -> (SocketAddr, Arc<Mutex<Vec<CapturedRequest>>>) {
        let captures: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let captures_for_app = captures.clone();
        let app = Router::new().fallback(move |req: Request<Body>| {
            let captures = captures_for_app.clone();
            async move {
                let (parts, body) = req.into_parts();
                let body_bytes = body::to_bytes(body, BODY_LIMIT).await.unwrap_or_default();
                let headers = parts
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect();
                let payload = serde_json::json!({
                    "mock_ok": true,
                    "method": parts.method.as_str(),
                    "path": parts.uri.path(),
                    "query": parts.uri.query(),
                });
                captures.lock().expect("captures lock").push(CapturedRequest {
                    method: parts.method.as_str().to_string(),
                    path: parts.uri.path().to_string(),
                    query: parts.uri.query().map(str::to_string),
                    headers,
                    body: body_bytes.to_vec(),
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("mock response")
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.unwrap();
        });
        (addr, captures)
    }

    struct ProviderFacadeHarness {
        addr: SocketAddr,
        rest_captures: Arc<Mutex<Vec<CapturedRequest>>>,
        mcp_captures: Arc<Mutex<Vec<CapturedRequest>>>,
        token: String,
        db_str: String,
        db_path: PathBuf,
    }

    impl Drop for ProviderFacadeHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.db_path);
        }
    }

    async fn spawn_provider_facade_harness(prefix: &str) -> ProviderFacadeHarness {
        spawn_provider_facade_harness_with_options(prefix, false, None).await
    }

    async fn spawn_provider_facade_harness_with_dev(
        prefix: &str,
        dev_open_admin: bool,
    ) -> ProviderFacadeHarness {
        spawn_provider_facade_harness_with_options(prefix, dev_open_admin, None).await
    }

    async fn spawn_provider_facade_harness_with_options(
        prefix: &str,
        dev_open_admin: bool,
        request_rate_limit: Option<i64>,
    ) -> ProviderFacadeHarness {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let (rest_addr, rest_captures) = spawn_capturing_mock_upstream().await;
        let (mcp_addr, mcp_captures) = spawn_capturing_mock_upstream().await;
        let providers = tavily_hikari::ProviderBootstrap {
            serpapi_upstream: format!("http://{rest_addr}"),
            // Path-prefixed MCP upstream (amendment M1): the executor must
            // target /gw/mcp exactly, never the client-facing ingress path.
            serpapi_mcp_upstream: format!("http://{mcp_addr}/gw/mcp"),
            exa_upstream: format!("http://{rest_addr}"),
            brave_upstream: format!("http://{rest_addr}"),
            serpapi_keys: vec!["mock-serpapi-key".to_string()],
            exa_keys: vec!["mock-exa-key".to_string()],
            brave_keys: vec!["mock-brave-key".to_string()],
        };
        let proxy = TavilyProxy::with_providers_in_ha_mode(
            vec!["tvly-provider-facade-key".to_string()],
            &format!("http://{rest_addr}/mcp"),
            &db_str,
            tavily_hikari::TavilyProxyOptions::from_database_path(&db_str),
            providers,
            tavily_hikari::HaMode::Single,
        )
        .await
        .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some(prefix))
            .await
            .expect("create access token");
        if let Some(limit) = request_rate_limit {
            // The hourly any-request gate reads the requestRateLimit system
            // setting (hot-reload seam shared with /api/tavily).
            let mut settings = proxy.get_system_settings().await.expect("settings");
            settings.request_rate_limit = limit;
            proxy
                .set_system_settings(&settings)
                .await
                .expect("set request rate limit");
        }
        let state = Arc::new(AppState {
            proxy,
            static_dir: None,
            forward_auth: ForwardAuthConfig::new(None, None, None, None),
            forward_auth_enabled: true,
            builtin_admin: BuiltinAdminAuth::new(false, None, None),
            admin_passkey: AdminPasskeyOptions::disabled(),
            linuxdo_oauth: LinuxDoOAuthOptions::disabled(),
            linuxdo_credit: LinuxDoCreditOptions::disabled(),
            ha: tavily_hikari::HaRuntime::new(tavily_hikari::HaConfig::default()),
            dev_open_admin,
            usage_base: format!("http://{rest_addr}"),
            api_key_ip_geo_origin: "https://api.country.is".to_string(),
            dashboard_overview_cache: new_dashboard_overview_cache(),
            remote_attempt_admission: new_remote_attempt_admission(),
        });
        // Same registration the production router merges in serve.rs, plus an
        // SPA-shaped HTML fallback so the M16 "never a 302" guarantee is
        // observable on a live router.
        let app = Router::new()
            .merge(provider_facade::provider_facade_routes())
            .fallback(|req: Request<Body>| async move {
                let accepts_html = req
                    .headers()
                    .get(axum::http::header::ACCEPT)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.to_ascii_lowercase().contains("text/html"))
                    .unwrap_or(false);
                if accepts_html {
                    Redirect::temporary("/__404").into_response()
                } else {
                    (StatusCode::NOT_FOUND, Body::empty()).into_response()
                }
            })
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.unwrap();
        });
        ProviderFacadeHarness {
            addr,
            rest_captures,
            mcp_captures,
            token: access_token.token,
            db_str,
            db_path,
        }
    }

    #[test]
    fn resolver_table_covers_every_registered_facade_route() {
        // path, provider, native upstream path, canonical kind, upstream_operation
        let expected = [
            (
                "/api/serpapi/search",
                ProviderId::Serpapi,
                "/search",
                "api:serpapi/search",
                "serpapi_http_search",
                true,
                true,
            ),
            (
                "/api/exa/search",
                ProviderId::Exa,
                "/search",
                "api:exa/search",
                "exa_http_search",
                false,
                true,
            ),
            (
                "/api/exa/contents",
                ProviderId::Exa,
                "/contents",
                "api:exa/contents",
                "exa_http_contents",
                false,
                true,
            ),
            (
                "/api/exa/answer",
                ProviderId::Exa,
                "/answer",
                "api:exa/answer",
                "exa_http_answer",
                false,
                true,
            ),
            (
                "/api/brave/res/v1/web/search",
                ProviderId::Brave,
                "/res/v1/web/search",
                "api:brave/web",
                "brave_http_web_search",
                true,
                false,
            ),
            (
                "/api/brave/res/v1/news/search",
                ProviderId::Brave,
                "/res/v1/news/search",
                "api:brave/news",
                "brave_http_news_search",
                true,
                false,
            ),
            (
                "/api/brave/res/v1/llm/context",
                ProviderId::Brave,
                "/res/v1/llm/context",
                "api:brave/llm-context",
                "brave_http_llm_context",
                true,
                false,
            ),
        ];
        for (path, provider, upstream_path, kind_key, operation, allow_get, allow_post) in expected {
            let route = provider_facade::resolve_provider_rest_route(path)
                .unwrap_or_else(|| panic!("resolver must cover {path}"));
            assert_eq!(route.provider, provider, "{path}");
            assert_eq!(route.upstream_path, upstream_path, "{path}");
            assert_eq!(route.upstream_operation, operation, "{path}");
            // The route table's operation label must equal the lib-side
            // provider-scoped vocabulary (prevents drift between the two).
            assert_eq!(
                tavily_hikari::provider_rest_upstream_operation(provider, upstream_path),
                Some(operation),
                "{path}"
            );
            assert_eq!(route.allow_get, allow_get, "{path}");
            assert_eq!(route.allow_post, allow_post, "{path}");
            assert_eq!(
                classify_token_request_kind(route.kind_path, None).key,
                kind_key,
                "{path}"
            );
            assert_eq!(provider_facade::resolve_provider_for_path(path), Some(provider));
        }
        // MCP ingress entries agree with the lib-side ingress helper.
        for provider in [ProviderId::Serpapi, ProviderId::Exa, ProviderId::Brave] {
            let ingress = provider_facade::resolve_provider_mcp_ingress(provider);
            assert_eq!(provider_facade::resolve_provider_mcp_route(ingress), Some(provider));
            assert_eq!(provider_facade::resolve_provider_for_path(ingress), Some(provider));
            assert!(tavily_hikari::is_mcp_ingress_path(provider, ingress));
            assert_eq!(
                provider_facade::resolve_provider_for_path(&format!("{ingress}/messages")),
                Some(provider)
            );
        }
    }

    #[test]
    fn resolver_rejects_unknown_and_legacy_tavily_paths() {
        for path in [
            "/api/tavily/search",
            "/mcp",
            "/mcp/messages",
            "/api/serpapi/searchx",
            "/api/serpapi",
            "/serpapi/mcpx",
            "/serpapi",
            "/exa/mcp-messages",
            "/api/brave/res/v1/web/searchx",
            "/",
            "",
        ] {
            assert_eq!(
                provider_facade::resolve_provider_for_path(path),
                None,
                "{path} must stay out of the provider resolver"
            );
        }
    }

    #[tokio::test]
    async fn serpapi_search_accepts_bearer_and_query_slot_and_strips_client_slot() {
        let harness = spawn_provider_facade_harness("provider-facade-serpapi-auth").await;
        let client = Client::new();

        // Universal Bearer slot.
        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=rust&engine=google",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("bearer request");
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(body.get("mock_ok").and_then(Value::as_bool), Some(true));
        assert_eq!(body.get("path").and_then(Value::as_str), Some("/search"));

        // Provider-native query slot (SerpApi api_key) carrying the Hikari token.
        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=rust&engine=google&api_key={}",
                harness.addr, harness.token
            ))
            .send()
            .await
            .expect("slot request");
        assert_eq!(resp.status(), StatusCode::OK);

        let captures = harness.rest_captures.lock().expect("captures lock");
        assert_eq!(captures.len(), 2);
        for captured in captures.iter() {
            let query = captured.query.clone().expect("upstream query");
            // Pooled upstream key arrives in the native SerpApi slot ...
            assert!(query.contains("api_key=mock-serpapi-key"), "{query}");
            // ... and the client Hikari token is stripped before upstream (M17).
            assert!(!query.contains("th-"), "{query}");
            assert!(!query.contains(&harness.token), "{query}");
            assert!(!captured.has_header("authorization"));
            assert!(query.contains("q=rust") && query.contains("engine=google"));
        }
    }

    #[tokio::test]
    async fn exa_facades_accept_x_api_key_and_bearer_and_strip_client_header() {
        let harness = spawn_provider_facade_harness("provider-facade-exa-auth").await;
        let client = Client::new();

        let resp = client
            .post(format!("http://{}/api/exa/search", harness.addr))
            .header("x-api-key", harness.token.clone())
            .header("content-type", "application/json")
            .body(r#"{"query":"rust"}"#)
            .send()
            .await
            .expect("exa search request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .post(format!("http://{}/api/exa/contents", harness.addr))
            .header("authorization", format!("Bearer {}", harness.token))
            .header("content-type", "application/json")
            .body(r#"{"urls":["https://example.com"]}"#)
            .send()
            .await
            .expect("exa contents request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .post(format!("http://{}/api/exa/answer", harness.addr))
            .header("x-api-key", harness.token.clone())
            .header("content-type", "application/json")
            .body(r#"{"question":"why?"}"#)
            .send()
            .await
            .expect("exa answer request");
        assert_eq!(resp.status(), StatusCode::OK);

        let captures = harness.rest_captures.lock().expect("captures lock");
        assert_eq!(captures.len(), 3);
        let expected: [(&str, &str, &[u8]); 3] = [
            ("POST", "/search", br#"{"query":"rust"}"#),
            ("POST", "/contents", br#"{"urls":["https://example.com"]}"#),
            ("POST", "/answer", br#"{"question":"why?"}"#),
        ];
        for (captured, (method, path, body)) in captures.iter().zip(expected) {
            assert_eq!(captured.method, method);
            assert_eq!(captured.path, path);
            assert_eq!(captured.body, body, "client body forwarded verbatim");
            // Pooled key replaces the client slot in the SAME native header.
            assert_eq!(captured.header("x-api-key"), Some("mock-exa-key"));
            for (_, value) in captured.headers.iter() {
                assert!(!value.contains("th-"), "client token leaked upstream: {value}");
            }
            assert!(!captured.has_header("authorization"));
        }
    }

    #[tokio::test]
    async fn brave_web_search_accepts_subscription_token_and_bearer() {
        let harness = spawn_provider_facade_harness("provider-facade-brave-auth").await;
        let client = Client::new();

        let resp = client
            .get(format!(
                "http://{}/api/brave/res/v1/web/search?q=test&count=5",
                harness.addr
            ))
            .header("x-subscription-token", harness.token.clone())
            .send()
            .await
            .expect("brave token request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .get(format!(
                "http://{}/api/brave/res/v1/news/search?q=test",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("brave bearer request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .get(format!(
                "http://{}/api/brave/res/v1/llm/context?q=test",
                harness.addr
            ))
            .header("x-subscription-token", harness.token.clone())
            .send()
            .await
            .expect("brave llm context request");
        assert_eq!(resp.status(), StatusCode::OK);

        let captures = harness.rest_captures.lock().expect("captures lock");
        assert_eq!(captures.len(), 3);
        let expected_paths = [
            "/res/v1/web/search",
            "/res/v1/news/search",
            "/res/v1/llm/context",
        ];
        for (captured, path) in captures.iter().zip(expected_paths) {
            assert_eq!(captured.method, "GET");
            assert_eq!(captured.path, path);
            assert_eq!(
                captured.header("x-subscription-token"),
                Some("mock-brave-key")
            );
            for (_, value) in captured.headers.iter() {
                assert!(!value.contains("th-"), "client token leaked upstream: {value}");
            }
            assert!(!captured.has_header("authorization"));
        }
        assert!(captures[0]
            .query
            .clone()
            .expect("query")
            .contains("count=5"));
    }

    #[tokio::test]
    async fn client_x_project_id_never_reaches_provider_upstreams() {
        // Amendment M2: x-project-id is Tavily-only policy; a client-forged
        // value must be stripped for every provider REST facade.
        let harness = spawn_provider_facade_harness("provider-facade-m2-project-id").await;
        let client = Client::new();

        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=test",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .header("x-project-id", "forged-project")
            .header("user-agent", "client-marker/9.9")
            .send()
            .await
            .expect("serpapi request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .post(format!("http://{}/api/exa/search", harness.addr))
            .header("x-api-key", harness.token.clone())
            .header("x-project-id", "forged-project")
            .header("user-agent", "client-marker/9.9")
            .body("{}")
            .send()
            .await
            .expect("exa request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .get(format!(
                "http://{}/api/brave/res/v1/web/search?q=test",
                harness.addr
            ))
            .header("x-subscription-token", harness.token.clone())
            .header("x-project-id", "forged-project")
            .header("user-agent", "client-marker/9.9")
            .send()
            .await
            .expect("brave request");
        assert_eq!(resp.status(), StatusCode::OK);

        let captures = harness.rest_captures.lock().expect("captures lock");
        assert_eq!(captures.len(), 3);
        for captured in captures.iter() {
            assert!(
                !captured.has_header("x-project-id"),
                "x-project-id reached {}: {:?}",
                captured.path,
                captured.headers
            );
            // The client UA never relays (the egress HTTP client's own default
            // UA is the same policy as the Tavily executors; allowlist-level
            // stripping is pinned by the lib sanitize-dispatch test).
            assert_ne!(
                captured.header("user-agent"),
                Some("client-marker/9.9"),
                "client user-agent relayed to {}: {:?}",
                captured.path,
                captured.headers
            );
        }
    }

    #[tokio::test]
    async fn foreign_and_missing_tokens_get_hikari_typed_401_shapes() {
        let harness = spawn_provider_facade_harness("provider-facade-foreign-token").await;
        let client = Client::new();

        // An Exa-shaped provider key against the Brave facade: Hikari 401,
        // never a provider-shaped error body.
        let resp = client
            .get(format!(
                "http://{}/api/brave/res/v1/web/search?q=test",
                harness.addr
            ))
            .header("x-subscription-token", "exa-shaped-not-a-hikari-token")
            .send()
            .await
            .expect("foreign token request");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid or disabled token")
        );
        assert_eq!(body.as_object().expect("object").len(), 1);

        // SerpApi native slot with a foreign key.
        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=test&api_key=not-a-hikari-token",
                harness.addr
            ))
            .send()
            .await
            .expect("foreign slot request");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid or disabled token")
        );

        // Missing token on every facade family keeps the parity shape.
        let resp = client
            .post(format!("http://{}/api/exa/search", harness.addr))
            .body("{}")
            .send()
            .await
            .expect("missing token request");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("missing token")
        );

        // Nothing reached the upstream mocks.
        assert!(harness.rest_captures.lock().expect("captures lock").is_empty());
    }

    #[tokio::test]
    async fn serpapi_mcp_route_passes_through_with_bearer_injection() {
        let harness = spawn_provider_facade_harness("provider-facade-serpapi-mcp").await;
        let client = Client::new();
        let resp = client
            .post(format!("http://{}/serpapi/mcp", harness.addr))
            .header("authorization", format!("Bearer {}", harness.token))
            .header("content-type", "application/json")
            .body(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            )
            .send()
            .await
            .expect("serpapi mcp request");
        // Todo-15 contract (replaces the documented T11 `provider_mcp_pending`
        // boundary placeholder, orchestrator-approved): the hosted passthrough
        // calls the upstream MCP base and its body passes through verbatim.
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(body.get("mock_ok"), Some(&Value::Bool(true)));

        // Amendment M1: the path-prefixed SERPAPI_MCP_UPSTREAM is targeted
        // exactly (/gw/mcp), never the client-facing ingress path. The guard
        // is scoped so it never lives across the awaits below.
        {
            let captures = harness.mcp_captures.lock().expect("captures lock");
            assert_eq!(captures.len(), 1, "exactly one upstream MCP call");
            let capture = &captures[0];
            assert_eq!(capture.method, "POST");
            assert_eq!(capture.path, "/gw/mcp");
            // The pooled key rides Authorization: Bearer; the Tavily-only
            // client slots never appear on serpapi outbound (header or query).
            assert_eq!(
                capture.header("authorization"),
                Some("Bearer mock-serpapi-key"),
                "pooled serpapi key injected as Bearer"
            );
            assert!(!capture.has_header("tavily-api-key"));
            let query = capture.query.clone().unwrap_or_default();
            assert!(!query.to_ascii_lowercase().contains("tavilyapikey"));
        }
        assert!(harness.rest_captures.lock().expect("captures lock").is_empty());

        // Dev-open-admin parity with /mcp: explicit token required.
        let dev_harness =
            spawn_provider_facade_harness_with_dev("provider-facade-serpapi-mcp-dev", true).await;
        let resp = client
            .post(format!("http://{}/serpapi/mcp", dev_harness.addr))
            .header("content-type", "application/json")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .send()
            .await
            .expect("dev open admin request");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("explicit_token_required")
        );
    }

    #[tokio::test]
    async fn exa_mcp_accepts_native_header_and_exa_api_key_query_slot() {
        let harness = spawn_provider_facade_harness("provider-facade-exa-mcp").await;
        let client = Client::new();

        // Todo-17 contract: the typed pending boundary is replaced by the exa
        // LOCAL facade — every response is an `event: message\ndata: {..}` SSE
        // frame carrying ONE JSON-RPC payload (session-less, local answers).
        let parse_sse = |text: &str| -> Value {
            let data = text
                .lines()
                .find_map(|line| line.strip_prefix("data:"))
                .unwrap_or_else(|| panic!("expected an SSE data frame, got: {text:?}"));
            serde_json::from_str(data.trim()).expect("sse payload decodes")
        };

        // MCP query slot exaApiKey (plan todo 11 auth matrix) authenticates;
        // initialize is answered locally with the exa facade identity.
        let resp = client
            .post(format!(
                "http://{}/exa/mcp?exaApiKey={}",
                harness.addr, harness.token
            ))
            .header("content-type", "application/json")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .send()
            .await
            .expect("exa mcp query slot request");
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().await.expect("response body");
        let payload = parse_sse(&text);
        assert_eq!(
            payload["result"]["serverInfo"]["name"],
            serde_json::json!("exa-search-hikari"),
            "todo-17 exa local facade identity"
        );

        // Native x-api-key header slot authenticates on the MCP surface too;
        // tools/list advertises the local catalog of EXACTLY three tools.
        let resp = client
            .post(format!("http://{}/exa/mcp", harness.addr))
            .header("x-api-key", harness.token.clone())
            .header("content-type", "application/json")
            .body(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .send()
            .await
            .expect("exa mcp header request");
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().await.expect("response body");
        let payload = parse_sse(&text);
        let mut tool_names: Vec<String> = payload["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
            .collect();
        tool_names.sort();
        assert_eq!(
            tool_names,
            vec![
                "exa_answer".to_string(),
                "web_fetch_exa".to_string(),
                "web_search_exa".to_string(),
            ],
            "the exa facade catalog is exactly the three todo-17 tools"
        );

        // tools/call dispatches through the pooled-key REST translation: the
        // upstream mock sees POST /search with the pooled key in x-api-key.
        let resp = client
            .post(format!("http://{}/exa/mcp", harness.addr))
            .header("authorization", format!("Bearer {}", harness.token))
            .header("content-type", "application/json")
            .body(
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_search_exa","arguments":{"query":"routing probe"}}}"#,
            )
            .send()
            .await
            .expect("exa mcp tools call request");
        assert_eq!(resp.status(), StatusCode::OK);
        let text = resp.text().await.expect("response body");
        let payload = parse_sse(&text);
        assert!(
            payload["result"]["isError"].is_null()
                || payload["result"]["isError"] == serde_json::json!(false),
            "translated tools/call succeeds against the capturing mock: {payload}"
        );
        {
            let captures = harness.rest_captures.lock().expect("captures lock");
            assert_eq!(captures.len(), 1, "exactly one upstream REST call");
            let capture = &captures[0];
            assert_eq!(capture.method, "POST");
            assert_eq!(capture.path, "/search");
            assert_eq!(
                capture.header("x-api-key"),
                Some("mock-exa-key"),
                "pooled exa key injected into the native header"
            );
            assert!(!capture.has_header("authorization"));
            let body = String::from_utf8_lossy(&capture.body);
            assert!(
                body.contains("\"query\":\"routing probe\""),
                "translated REST body carries the tool arguments: {body}"
            );
            assert!(!body.contains("th-"), "client token never forwarded");
        }

        // Foreign exaApiKey stays a Hikari 401.
        let resp = client
            .post(format!("http://{}/exa/mcp?exaApiKey=foreign", harness.addr))
            .header("content-type", "application/json")
            .body(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .send()
            .await
            .expect("exa mcp foreign request");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("invalid or disabled token")
        );
    }

    #[tokio::test]
    async fn provider_mcp_subpath_reject_is_provider_tagged_and_never_spa_redirect() {
        let harness = spawn_provider_facade_harness("provider-facade-m16-reject").await;
        let client = Client::new();

        // HTML-accepting client: the registered reject route must win over the
        // SPA 302 fallback (amendment M16).
        let resp = client
            .post(format!(
                "http://{}/serpapi/mcp/messages?api_key={}&q=test",
                harness.addr, harness.token
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .header("accept", "text/html,application/xhtml+xml")
            .header("content-type", "application/json")
            .body(r#"{"probe":"raw-body"}"#)
            .send()
            .await
            .expect("serpapi subpath request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(
            resp.headers().get("location").is_none(),
            "must never redirect to the SPA 404 landing"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(resp.text().await.expect("body"), "Not Found");

        // Exa + Brave subpaths follow the same reject behavior.
        let resp = client
            .get(format!("http://{}/exa/mcp/whatever", harness.addr))
            .header("x-api-key", harness.token.clone())
            .send()
            .await
            .expect("exa subpath request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = client
            .get(format!("http://{}/brave/mcp/whatever", harness.addr))
            .header("x-subscription-token", harness.token.clone())
            .send()
            .await
            .expect("brave subpath request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Null-key raw mcp_path_404 logs are provider-tagged (M16) with the
        // client auth slot stripped from the persisted query (M17).
        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = sqlx::query(
            "SELECT provider, failure_kind, result_status, api_key_id, path, query \
             FROM request_logs ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("request logs");
        assert_eq!(rows.len(), 3);
        let expected = [
            ("serpapi", "/serpapi/mcp/messages", Some("q=test")),
            ("exa", "/exa/mcp/whatever", None),
            ("brave", "/brave/mcp/whatever", None),
        ];
        for (row, (provider, path, query)) in rows.iter().zip(expected) {
            assert_eq!(row.get::<Option<String>, _>("provider").as_deref(), Some(provider));
            assert_eq!(
                row.get::<Option<String>, _>("failure_kind").as_deref(),
                Some("mcp_path_404")
            );
            assert_eq!(
                row.get::<Option<String>, _>("result_status").as_deref(),
                Some("error")
            );
            assert!(row.get::<Option<String>, _>("api_key_id").is_none());
            assert_eq!(row.get::<String, _>("path"), path);
            assert_eq!(row.get::<Option<String>, _>("query").as_deref(), query);
        }
        pool.close().await;
    }

    #[tokio::test]
    async fn persisted_reject_rows_contain_no_client_tokens() {
        // Amendment M17: neither th- tokens nor provider key values survive
        // into persisted query text.
        let harness = spawn_provider_facade_harness("provider-facade-m17-persist").await;
        let client = Client::new();
        let resp = client
            .post(format!(
                "http://{}/serpapi/mcp/messages?api_key={}&q=secretless",
                harness.addr, harness.token
            ))
            .header("content-type", "application/json")
            .body(r#"{"probe":"m17"}"#)
            .send()
            .await
            .expect("reject request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let row = sqlx::query(
            "SELECT path, query, CAST(request_body AS TEXT) AS body_text \
             FROM request_logs WHERE failure_kind = 'mcp_path_404'",
        )
        .fetch_one(&pool)
        .await
        .expect("reject row");
        let query = row.get::<Option<String>, _>("query").unwrap_or_default();
        assert_eq!(query, "q=secretless");
        assert!(!query.contains("th-"));
        assert!(!query.contains(&harness.token));
        let body_text = row.get::<Option<String>, _>("body_text").unwrap_or_default();
        assert!(body_text.contains(r#"{"probe":"m17"}"#), "raw body kept");
        assert!(!body_text.contains(&harness.token));

        let token_log_leaks = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM auth_token_logs WHERE query LIKE '%th-%'",
        )
        .fetch_one(&pool)
        .await
        .expect("token log scan");
        assert_eq!(token_log_leaks, 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn provider_facade_hourly_limit_gate_rejects_at_parity() {
        // requestRateLimit=1: the shared in-memory any-request limiter (same
        // seam as /api/tavily) must reject the 2nd request with the Hikari
        // quota_exhausted / hourlyAny shape.
        let harness =
            spawn_provider_facade_harness_with_options("provider-facade-hourly-gate", false, Some(1))
                .await;
        let client = Client::new();

        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=one",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("first request");
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = client
            .get(format!(
                "http://{}/api/serpapi/search?q=two",
                harness.addr
            ))
            .header("authorization", format!("Bearer {}", harness.token))
            .send()
            .await
            .expect("second request");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("quota_exhausted")
        );
        assert!(body.get("hourlyAny").is_some(), "{body}");

        // Only the first request reached the upstream.
        let captures = harness.rest_captures.lock().expect("captures lock");
        assert_eq!(captures.len(), 1);
    }

    // ---------------------------------------------------------------------
    // Bounded cross-key retry seam coverage (plan provider-key-429-retry todo 9).
    //
    // The provider REST facades share the todo-6 Tavily HTTP retry contract: at
    // most ONE extra upstream attempt per logical request, zero sleep, one
    // visible `request_logs` row per attempt marked through `fallback_reason`,
    // one billing settlement on whatever the chain returns, and a byte-for-byte
    // passthrough of the buffered attempt whenever the kernel says
    // `ServeOriginal`. What this battery adds is the PROVIDER dimension: the two
    // Exa vetoes (429 and team-scoped 402 keep a single attempt even with a
    // willing second key and a willing mock) against a KEY-scoped Exa 402, which
    // does rotate.
    //
    // Every case seeds a TWO-key pool and a mock that would happily serve a
    // second attempt, so a single-attempt outcome can only be explained by
    // policy - never by an empty pool or an unwilling upstream.
    //
    // rustfmt never visits this file (it is declared from an `include!`d
    // `tests.rs`), so the four-space module indentation below is kept by hand.
    // ---------------------------------------------------------------------

    /// SerpApi's hourly-flavor 429 body: classifies `RateLimited`, never
    /// `Exhausted` (the run-out flavor is the todo-11 battery's row).
    const FACADE_RETRY_SERPAPI_HOURLY_BODY: &str =
        r#"{"error":"Your account has exceeded its hourly throughput limit."}"#;
    /// Exa 429 body. `RATE_LIMIT_EXCEEDED` is NOT a budget tag, so this veto
    /// comes from the rate-limit family, not from tag evidence.
    const FACADE_RETRY_EXA_RATE_LIMITED_BODY: &str =
        r#"{"requestId":"mock","error":"Rate limit exceeded","tag":"RATE_LIMIT_EXCEEDED"}"#;
    /// Exa team-scoped 402: another key cannot lift a team budget, so the kernel
    /// vetoes the rotation.
    const FACADE_RETRY_EXA_TEAM_BUDGET_BODY: &str =
        r#"{"requestId":"mock","error":"team budget spent","tag":"TEAM_BUDGET_EXCEEDED"}"#;
    /// Exa KEY-scoped 402: this key's own prepaid credits ran out, so another key
    /// can serve the request.
    const FACADE_RETRY_EXA_KEY_BUDGET_BODY: &str =
        r#"{"requestId":"mock","error":"no more credits","tag":"NO_MORE_CREDITS"}"#;
    /// Body the mock returns on every attempt after a rejected first one.
    /// Distinct from each rejection body so the client-visible bytes prove WHICH
    /// attempt was served.
    const FACADE_RETRY_SECOND_HIT_BODY: &str =
        r#"{"organic_results":[{"title":"second key hit"}],"requestId":"mock"}"#;

    /// One provider REST upstream attempt as the mock saw it: the upstream path,
    /// the pooled key that reached it, and the raw query. Recording the query is
    /// what lets a case prove the per-attempt URL rebuild injected the native key
    /// slot exactly ONCE.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FacadeRetryAttempt {
        path: String,
        api_key: String,
        query: Option<String>,
    }

    /// Mock behaviour: which upstream path rejects on its first attempt, and
    /// with what response. Later attempts on that path always succeed.
    #[derive(Debug, Clone, Copy)]
    struct FacadeRetryMockPlan {
        path: &'static str,
        first_status: u16,
        first_retry_after: Option<&'static str>,
        first_body: &'static str,
    }

    impl FacadeRetryMockPlan {
        /// The rotation shape: the first attempt on `path` answers the rejection
        /// and every later attempt succeeds, so ONLY the retry policy explains
        /// what the client ends up seeing.
        fn rotating_after_rejection(
            path: &'static str,
            first_status: u16,
            first_retry_after: Option<&'static str>,
            first_body: &'static str,
        ) -> Self {
            Self {
                path,
                first_status,
                first_retry_after,
                first_body,
            }
        }
    }

    /// JSON response with an optional Retry-After header.
    fn facade_retry_json_response(status: u16, retry_after: Option<&str>, body: &str) -> Response {
        let mut builder =
            Response::builder().status(status).header(CONTENT_TYPE, "application/json");
        if let Some(retry_after) = retry_after {
            builder = builder.header(axum::http::header::RETRY_AFTER, retry_after);
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("build mock response")
    }

    /// The pooled key an upstream attempt carried, read out of whichever native
    /// slot its provider uses: SerpApi's `api_key` query slot, Exa's `x-api-key`
    /// header, Brave's `x-subscription-token` header. The seeded mock keys are
    /// URL-safe, so the raw query pair is the exact value the executor appended.
    fn facade_retry_upstream_key(query: Option<&str>, headers: &HeaderMap) -> String {
        if let Some(query) = query {
            for pair in query.split('&') {
                if let Some(value) = pair.strip_prefix("api_key=") {
                    return value.to_string();
                }
            }
        }
        for name in ["x-api-key", "x-subscription-token"] {
            if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
                return value.to_string();
            }
        }
        String::new()
    }

    /// Router serving every provider REST upstream path under `plan` and
    /// recording each attempt. One mock covers SerpApi (`/search` GET), Exa
    /// (`/search` POST) and Brave (`/res/v1/*` GET) alike, so a case only names
    /// the upstream path it rejects on.
    fn facade_retry_mock_router(
        plan: FacadeRetryMockPlan,
        attempts: Arc<Mutex<Vec<FacadeRetryAttempt>>>,
    ) -> Router {
        Router::new().fallback(move |req: Request<Body>| {
            let attempts = attempts.clone();
            async move {
                let (parts, body) = req.into_parts();
                let _ = body::to_bytes(body, BODY_LIMIT).await.unwrap_or_default();
                let path = parts.uri.path().to_string();
                let query = parts.uri.query().map(str::to_string);
                let api_key = facade_retry_upstream_key(parts.uri.query(), &parts.headers);
                let attempt = {
                    let mut seen = attempts.lock().expect("attempts lock poisoned");
                    let attempt = seen.iter().filter(|entry| entry.path == path).count() + 1;
                    seen.push(FacadeRetryAttempt {
                        path: path.clone(),
                        api_key,
                        query,
                    });
                    attempt
                };
                if path == plan.path && attempt == 1 {
                    return facade_retry_json_response(
                        plan.first_status,
                        plan.first_retry_after,
                        plan.first_body,
                    );
                }
                facade_retry_json_response(200, None, FACADE_RETRY_SECOND_HIT_BODY)
            }
        })
    }

    struct ProviderFacadeRetryHarness {
        addr: SocketAddr,
        attempts: Arc<Mutex<Vec<FacadeRetryAttempt>>>,
        token: String,
        token_id: String,
        db_str: String,
        db_path: PathBuf,
    }

    impl Drop for ProviderFacadeRetryHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.db_path);
        }
    }

    /// Two-key-per-provider pool in front of a plan-driven mock upstream. The
    /// second key is what makes a rotation possible at all, so a single-attempt
    /// outcome is always a policy verdict and never an empty pool.
    async fn spawn_provider_facade_retry_harness(
        prefix: &str,
        plan: FacadeRetryMockPlan,
    ) -> ProviderFacadeRetryHarness {
        let db_path = temp_db_path(prefix);
        let db_str = db_path.to_string_lossy().to_string();
        let attempts: Arc<Mutex<Vec<FacadeRetryAttempt>>> = Arc::new(Mutex::new(Vec::new()));
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_app = facade_retry_mock_router(plan, attempts.clone());
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_app.into_make_service())
                .await
                .unwrap();
        });
        let upstream = format!("http://{upstream_addr}");
        let providers = tavily_hikari::ProviderBootstrap {
            serpapi_upstream: upstream.clone(),
            // Path-prefixed MCP upstream, as in the routing harness: the retry
            // seam must never touch it.
            serpapi_mcp_upstream: format!("{upstream}/gw/mcp"),
            exa_upstream: upstream.clone(),
            brave_upstream: upstream.clone(),
            serpapi_keys: vec![
                "mock-serpapi-key-a".to_string(),
                "mock-serpapi-key-b".to_string(),
            ],
            exa_keys: vec![
                "mock-exa-key-a".to_string(),
                "mock-exa-key-b".to_string(),
            ],
            brave_keys: vec![
                "mock-brave-key-a".to_string(),
                "mock-brave-key-b".to_string(),
            ],
        };
        let proxy = TavilyProxy::with_providers_in_ha_mode(
            vec!["tvly-facade-retry-key".to_string()],
            &format!("{upstream}/mcp"),
            &db_str,
            tavily_hikari::TavilyProxyOptions::from_database_path(&db_str),
            providers,
            tavily_hikari::HaMode::Single,
        )
        .await
        .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some(prefix))
            .await
            .expect("create access token");
        let state = Arc::new(AppState {
            proxy,
            static_dir: None,
            forward_auth: ForwardAuthConfig::new(None, None, None, None),
            forward_auth_enabled: true,
            builtin_admin: BuiltinAdminAuth::new(false, None, None),
            admin_passkey: AdminPasskeyOptions::disabled(),
            linuxdo_oauth: LinuxDoOAuthOptions::disabled(),
            linuxdo_credit: LinuxDoCreditOptions::disabled(),
            ha: tavily_hikari::HaRuntime::new(tavily_hikari::HaConfig::default()),
            dev_open_admin: false,
            usage_base: upstream.clone(),
            api_key_ip_geo_origin: "https://api.country.is".to_string(),
            dashboard_overview_cache: new_dashboard_overview_cache(),
            remote_attempt_admission: new_remote_attempt_admission(),
        });
        let app = Router::new()
            .merge(provider_facade::provider_facade_routes())
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.unwrap();
        });
        ProviderFacadeRetryHarness {
            addr,
            attempts,
            token: access_token.token,
            token_id: access_token.id,
            db_str,
            db_path,
        }
    }

    /// One `observability.request_logs` row of a provider facade attempt, in the
    /// order the attempts ran.
    #[derive(Debug)]
    struct FacadeRetryLogRow {
        provider: Option<String>,
        status_code: Option<i64>,
        result_status: String,
        fallback_reason: Option<String>,
        api_key_id: Option<String>,
    }

    async fn fetch_facade_retry_request_log_rows(
        pool: &sqlx::SqlitePool,
    ) -> Vec<FacadeRetryLogRow> {
        let rows = sqlx::query(
            r#"
            SELECT provider, status_code, result_status, fallback_reason, api_key_id
            FROM observability.request_logs
            ORDER BY id ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .expect("fetch request log rows");
        rows.iter()
            .map(|row| FacadeRetryLogRow {
                provider: row.try_get::<Option<String>, _>("provider").unwrap(),
                status_code: row.try_get::<Option<i64>, _>("status_code").unwrap(),
                result_status: row.try_get::<String, _>("result_status").unwrap(),
                fallback_reason: row
                    .try_get::<Option<String>, _>("fallback_reason")
                    .unwrap(),
                api_key_id: row.try_get::<Option<String>, _>("api_key_id").unwrap(),
            })
            .collect()
    }

    /// One `auth_token_logs` row. Billing settles once per logical request, so a
    /// retry chain must never produce more than one - and it must describe the
    /// FINAL attempt.
    #[derive(Debug)]
    struct FacadeRetryTokenLog {
        result_status: String,
        business_credits: Option<i64>,
        http_status: Option<i64>,
    }

    async fn fetch_facade_retry_token_logs(
        pool: &sqlx::SqlitePool,
        token_id: &str,
    ) -> Vec<FacadeRetryTokenLog> {
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
            .map(|row| FacadeRetryTokenLog {
                result_status: row.try_get::<String, _>("result_status").unwrap(),
                business_credits: row.try_get::<Option<i64>, _>("business_credits").unwrap(),
                http_status: row.try_get::<Option<i64>, _>("http_status").unwrap(),
            })
            .collect()
    }

    /// `api_keys.status` of one key.
    async fn fetch_facade_retry_key_status(pool: &sqlx::SqlitePool, key_id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM api_keys WHERE id = ?")
            .bind(key_id)
            .fetch_one(pool)
            .await
            .expect("fetch api key status")
    }

    /// Transient backoffs armed for one key as `(scope, reason_code)`. Empty
    /// means the key was never cooled.
    async fn fetch_facade_retry_armed_backoffs(
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

    /// The recorded upstream attempts, in order.
    fn facade_retry_seen(harness: &ProviderFacadeRetryHarness) -> Vec<FacadeRetryAttempt> {
        harness.attempts.lock().expect("attempts lock poisoned").clone()
    }

    /// Asserts the two-attempt rotation shape shared by the happy cases: the
    /// mock saw exactly two attempts on `path`, on two DIFFERENT keys of the
    /// seeded pool, each with the native key slot injected exactly once.
    fn assert_facade_retry_rotated(seen: &[FacadeRetryAttempt], path: &str, pool_keys: [&str; 2]) {
        assert_eq!(seen.len(), 2, "one retry, never more: {seen:?}");
        assert!(
            seen.iter().all(|entry| entry.path == path),
            "every attempt must target {path}: {seen:?}"
        );
        assert_ne!(
            seen[0].api_key, seen[1].api_key,
            "the retry must rotate to another key: {seen:?}"
        );
        assert!(
            seen.iter()
                .all(|entry| pool_keys.contains(&entry.api_key.as_str())),
            "only seeded pool keys may reach upstream: {seen:?}"
        );
        for entry in seen {
            let query = entry.query.clone().unwrap_or_default();
            // SerpApi carries the pooled key in its native `api_key` query slot;
            // Exa and Brave carry it in a header, so their query has none. Either
            // way the per-attempt URL rebuild must inject the slot at most once.
            assert!(
                query.matches("api_key=").count() <= 1,
                "the per-attempt URL rebuild must never stack native key slots: {query}"
            );
        }
    }

    #[tokio::test]
    async fn serpapi_facade_429_rotates_to_a_second_key_and_settles_once() {
        // Retry-After: 0 sits inside the kernel's 2s fast-path window, so the
        // retry is immediate and the chain never sleeps.
        let harness = spawn_provider_facade_retry_harness(
            "provider-facade-retry-serpapi-429",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                429,
                Some("0"),
                FACADE_RETRY_SERPAPI_HOURLY_BODY,
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

        // The client sees the RETRY's response byte-for-byte; the buffered 429
        // never reaches it.
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_SECOND_HIT_BODY.as_bytes());

        assert_facade_retry_rotated(
            &facade_retry_seen(&harness),
            "/search",
            ["mock-serpapi-key-a", "mock-serpapi-key-b"],
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;

        // One visible request_logs row per attempt, marked attempted ->
        // succeeded, each naming the key that actually served it.
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("serpapi"));
        assert_eq!(rows[1].provider.as_deref(), Some("serpapi"));
        assert_eq!(rows[0].status_code, Some(429), "first hit row keeps the 429");
        assert_eq!(
            rows[0].result_status, "error",
            "the hourly 429 flavor is an error, never exhaustion"
        );
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
        assert_ne!(
            rows[0].api_key_id, rows[1].api_key_id,
            "the two rows must name two different pool keys: {rows:?}"
        );

        // Billing settles ONCE, on the final response: a single auth_token_logs
        // row charging the retry's single search credit.
        let token_logs = fetch_facade_retry_token_logs(&pool, &harness.token_id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].http_status, Some(200));
        assert_eq!(
            token_logs[0].business_credits,
            Some(1),
            "the retry is not billed twice"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn exa_facade_429_is_vetoed_into_a_single_unmarked_attempt() {
        // Retry-After: 1 stays INSIDE the 2s fast-path window and the pool holds
        // a second key the mock would serve, so the ONLY thing that can explain a
        // single attempt is the Exa 429 veto (a team-level limit another key
        // cannot lift).
        let harness = spawn_provider_facade_retry_harness(
            "provider-facade-retry-exa-429-veto",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                429,
                Some("1"),
                FACADE_RETRY_EXA_RATE_LIMITED_BODY,
            ),
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

        // Byte-for-byte passthrough of the vetoed first hit: status, the upstream
        // Retry-After and the body all survive, and the 502 path is never reused.
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1"),
            "the upstream Retry-After header must be preserved"
        );
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_EXA_RATE_LIMITED_BODY.as_bytes());

        let seen = facade_retry_seen(&harness);
        assert_eq!(seen.len(), 1, "a vetoed 429 never replays: {seen:?}");
        assert_eq!(seen[0].path, "/search");

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("exa"));
        assert_eq!(rows[0].status_code, Some(429));
        assert!(
            rows[0].fallback_reason.is_none(),
            "a veto is not a chain, so the row stays unmarked: {rows:?}"
        );

        // The veto stops ROTATION, not marking: the 429 still arms the shared
        // HTTP backoff scope on the key that was rejected.
        let key_id = rows[0].api_key_id.clone().expect("row names its key");
        let armed = fetch_facade_retry_armed_backoffs(&pool, &key_id).await;
        assert!(
            armed.iter().any(|(scope, _)| scope == "http_global"),
            "the vetoed 429 must still cool its key: {armed:?}"
        );

        // One logical request still settles exactly once (as an error).
        let token_logs = fetch_facade_retry_token_logs(&pool, &harness.token_id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request: {token_logs:?}"
        );
        assert_eq!(token_logs[0].http_status, Some(429));
        pool.close().await;
    }

    #[tokio::test]
    async fn exa_facade_team_budget_402_is_vetoed_into_a_single_attempt() {
        // TEAM_BUDGET_EXCEEDED is team-scoped: no other key in the pool can serve
        // the request, so the kernel vetoes the rotation even though the second
        // key is active and the mock would have answered it.
        let harness = spawn_provider_facade_retry_harness(
            "provider-facade-retry-exa-team-402-veto",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                402,
                None,
                FACADE_RETRY_EXA_TEAM_BUDGET_BODY,
            ),
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
        assert_eq!(body.as_ref(), FACADE_RETRY_EXA_TEAM_BUDGET_BODY.as_bytes());

        let seen = facade_retry_seen(&harness);
        assert_eq!(
            seen.len(),
            1,
            "a team-scoped 402 never rotates a key: {seen:?}"
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 1, "the rejected attempt stays visible: {rows:?}");
        assert_eq!(rows[0].status_code, Some(402));
        assert_eq!(rows[0].result_status, "quota_exhausted");
        assert!(
            rows[0].fallback_reason.is_none(),
            "a veto is not a chain, so the row stays unmarked: {rows:?}"
        );
        // Marking is untouched by the veto: the key is still flipped exhausted.
        let key_id = rows[0].api_key_id.clone().expect("row names its key");
        assert_eq!(
            fetch_facade_retry_key_status(&pool, &key_id).await,
            "exhausted",
            "the team-budget key is still marked exhausted"
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn exa_facade_key_scoped_402_rotates_to_a_second_key() {
        // NO_MORE_CREDITS is KEY-scoped: this key's own prepaid credits ran out,
        // so another key CAN serve the request and the veto must not fire.
        let harness = spawn_provider_facade_retry_harness(
            "provider-facade-retry-exa-key-402",
            FacadeRetryMockPlan::rotating_after_rejection(
                "/search",
                402,
                None,
                FACADE_RETRY_EXA_KEY_BUDGET_BODY,
            ),
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

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.bytes().await.expect("read response body");
        assert_eq!(body.as_ref(), FACADE_RETRY_SECOND_HIT_BODY.as_bytes());

        assert_facade_retry_rotated(
            &facade_retry_seen(&harness),
            "/search",
            ["mock-exa-key-a", "mock-exa-key-b"],
        );

        let pool = connect_sqlite_test_pool(&harness.db_str).await;
        let rows = fetch_facade_retry_request_log_rows(&pool).await;
        assert_eq!(rows.len(), 2, "one request_logs row per attempt: {rows:?}");
        assert_eq!(rows[0].provider.as_deref(), Some("exa"));
        assert_eq!(rows[0].status_code, Some(402));
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
        // The exhausted key stays exhausted: Exa recovery is success-only, so the
        // rotation was the ONLY way this request could have been served.
        let first_key_id = rows[0].api_key_id.clone().expect("row names its key");
        assert_eq!(
            fetch_facade_retry_key_status(&pool, &first_key_id).await,
            "exhausted"
        );

        let token_logs = fetch_facade_retry_token_logs(&pool, &harness.token_id).await;
        assert_eq!(
            token_logs.len(),
            1,
            "one settlement per logical request, never one per attempt: {token_logs:?}"
        );
        assert_eq!(token_logs[0].result_status, "success");
        assert_eq!(token_logs[0].http_status, Some(200));
        assert_eq!(
            token_logs[0].business_credits,
            Some(1),
            "exa search is one credit and the retry is not billed twice"
        );
        pool.close().await;
    }
