use super::*;
    use super::core_support_and_parsing::*;
    use tavily_hikari::providers::ProviderId;

    /// Keenable hosted passthrough (keenable-integration todo 8) + the typed
    /// todo-2 Zhihu boundary (todo 10 pending): an authenticated MCP dispatch
    /// on `/keenable/mcp` EXECUTES (upstream dial, typed 502 proxy-error
    /// boundary when the configured dead-port upstream refuses the connection
    /// — never a 200, never a key mark) while `/zhihu/mcp` keeps the typed
    /// `provider_mcp_pending` response — never a 200, never an upstream call,
    /// never a key mark. The dispatch is called directly with the provider id
    /// because todo 6 owns the route registration.
    #[tokio::test]
    async fn keenable_mcp_executes_while_zhihu_dispatch_keeps_typed_pending_boundary() {
        let db_path = temp_db_path("provider-mcp-pending-boundary");
        let db_str = db_path.to_string_lossy().to_string();
        let providers = tavily_hikari::ProviderBootstrap {
            // 127.0.0.1 stub bases only (stub-only rule); the boundary makes
            // no upstream call, so nothing ever dials them.
            serpapi_upstream: "http://127.0.0.1:1".to_string(),
            serpapi_mcp_upstream: "http://127.0.0.1:1/mcp".to_string(),
            exa_upstream: "http://127.0.0.1:1".to_string(),
            brave_upstream: "http://127.0.0.1:1".to_string(),
            serpapi_keys: vec!["mock-serpapi-key".to_string()],
            exa_keys: vec!["mock-exa-key".to_string()],
            brave_keys: vec!["mock-brave-key".to_string()],
            keenable_upstream: "http://127.0.0.1:1".to_string(),
            keenable_mcp_upstream: "http://127.0.0.1:1/mcp".to_string(),
            zhihu_upstream: "http://127.0.0.1:1".to_string(),
            keenable_keys: vec!["mock-keenable-key".to_string()],
            zhihu_keys: vec!["mock-zhihu-key".to_string()],
        };
        let proxy = TavilyProxy::with_providers_in_ha_mode(
            vec!["tvly-mcp-pending".to_string()],
            "http://127.0.0.1:1/mcp",
            &db_str,
            tavily_hikari::TavilyProxyOptions::from_database_path(&db_str),
            providers,
            tavily_hikari::HaMode::Single,
        )
        .await
        .expect("proxy created");
        let access_token = proxy
            .create_access_token(Some("provider-mcp-pending"))
            .await
            .expect("create access token");

        // Seed one key per provider so the "never a key mark" assertion is
        // observable: both rows must stay byte-identical after the boundary.
        let pool = connect_sqlite_test_pool(&db_str).await;
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            r#"
            INSERT INTO api_keys (id, api_key, provider, status, created_at)
            VALUES ('ken-mcp-pending-key', 'ken-mcp-pending', 'keenable', 'active', ?),
                   ('zh-mcp-pending-key', 'zh-mcp-pending', 'zhihu', 'active', ?)
            "#,
        )
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .expect("seed keenable/zhihu keys");

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
            usage_base: "http://127.0.0.1:1".to_string(),
            api_key_ip_geo_origin: "https://api.country.is".to_string(),
            dashboard_overview_cache: new_dashboard_overview_cache(),
            remote_attempt_admission: new_remote_attempt_admission(),
        });

        for (provider, ingress, expect_keenable_proxy_error) in [
            // Keenable EXECUTES: the initialize reaches the executor and the
            // configured dead local port refuses the connection, which the
            // passthrough surfaces as its typed proxy-error boundary
            // (Err(502)) - never a 200, never a silent success.
            (ProviderId::Keenable, "/keenable/mcp", true),
            // Zhihu keeps the typed todo-2 provider_mcp_pending boundary
            // until todo 10 lands (501 + typed body, zero upstream calls).
            (ProviderId::Zhihu, "/zhihu/mcp", false),
        ] {
            let request = Request::builder()
                .method(Method::POST)
                .uri(ingress)
                .header(CONTENT_TYPE, "application/json")
                .header("authorization", format!("Bearer {}", access_token.token))
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-03-26",
                            "capabilities": {},
                            "clientInfo": { "name": "pending-boundary", "version": "0" }
                        }
                    })
                    .to_string(),
                ))
                .expect("build mcp request");
            let dispatched =
                provider_facade::provider_mcp_facade(state.clone(), request, provider).await;
            if expect_keenable_proxy_error {
                let status = dispatched.expect_err("typed proxy-error boundary");
                assert_eq!(
                    status, StatusCode::BAD_GATEWAY,
                    "{ingress} typed proxy-error boundary status"
                );
            } else {
                let response = dispatched.expect("boundary response");
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_IMPLEMENTED,
                    "{ingress} boundary status"
                );
                let body = axum::body::to_bytes(response.into_body(), BODY_LIMIT)
                    .await
                    .expect("boundary body");
                let payload: serde_json::Value =
                    serde_json::from_slice(&body).expect("boundary json");
                assert_eq!(
                    payload.get("error").and_then(|value| value.as_str()),
                    Some("provider_mcp_pending"),
                    "{ingress} boundary body: {payload}"
                );
                assert!(payload
                    .get("message")
                    .and_then(|value| value.as_str())
                    .is_some_and(|message| message.contains(provider.as_str())));
            }
        }

        // Never a key mark: the Zhihu seed row stays byte-identical (its
        // boundary touches nothing) and the Keenable row keeps its `active`
        // status after the dead-port transport failure (a transport error
        // never marks the key; `last_used_at` legitimately advances because
        // the executor logs the attempt).
        async fn key_row_snapshot(
            pool: &sqlx::SqlitePool,
            key_row: &str,
        ) -> (String, i64, i64) {
            sqlx::query_as(
                "SELECT status, status_changed_at, last_used_at FROM api_keys WHERE id = ?",
            )
            .bind(key_row)
            .fetch_one(pool)
            .await
            .expect("provider key snapshot")
        }
        let zhihu_baseline = key_row_snapshot(&pool, "zh-mcp-pending-key").await;
        assert_eq!(zhihu_baseline.0, "active", "zhihu key starts active");
        let zhihu_after = key_row_snapshot(&pool, "zh-mcp-pending-key").await;
        assert_eq!(
            zhihu_after, zhihu_baseline,
            "zh-mcp-pending-key must stay byte-identical after the typed boundary"
        );
        let keenable_status = key_row_snapshot(&pool, "ken-mcp-pending-key").await;
        assert_eq!(
            keenable_status.0, "active",
            "ken-mcp-pending-key must stay active after the dead-port transport failure"
        );
        pool.close().await;

        let _ = std::fs::remove_file(db_path);
    }
