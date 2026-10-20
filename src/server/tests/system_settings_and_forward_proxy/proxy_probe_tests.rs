    // Forward-proxy probe classification/config tests (include! fragment).
    // Kept out of system_settings_and_forward_proxy.rs to stay within the Rust source line budget.

    /// Accepts TCP connections and immediately closes them, so forward-proxy
    /// probes fail at the transport layer without relying on wall-clock timing.
    async fn spawn_refusing_forward_proxy() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refusing forward proxy");
        let addr = listener.local_addr().expect("refusing proxy addr");
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => drop(stream),
                    Err(_) => return,
                }
            }
        });
        addr
    }

    /// Accepts TCP connections and never writes a response, so a probe can only
    /// finish through its timeout budget.
    async fn spawn_stalled_forward_proxy() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stalled forward proxy");
        let addr = listener.local_addr().expect("stalled proxy addr");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let _held = stream;
                    std::future::pending::<()>().await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn proxy_probe_validate_endpoint_accepts_502_response() {
        let db_path = temp_db_path("proxy-probe-validate-502");
        let db_str = db_path.to_string_lossy().to_string();
        let upstream_addr = spawn_forward_proxy_probe_upstream().await;
        let fake_proxy_addr = spawn_fake_forward_proxy(StatusCode::BAD_GATEWAY).await;
        let upstream = format!("http://{}/mcp", upstream_addr);
        let usage_base = format!("http://{}", upstream_addr);
        let proxy =
            TavilyProxy::with_endpoint::<Vec<String>, String>(Vec::new(), &upstream, &db_str)
                .await
                .expect("create proxy");
        let addr = spawn_admin_forward_proxy_server(proxy, usage_base, true).await;

        let client = Client::new();
        let response = client
            .post(format!("http://{addr}/api/settings/forward-proxy/validate"))
            .json(&serde_json::json!({
                "kind": "proxyUrl",
                "value": format!("http://{}", fake_proxy_addr),
            }))
            .send()
            .await
            .expect("validate proxy");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("decode validation");
        assert_eq!(
            body["ok"].as_bool(),
            Some(true),
            "any HTTP response through the proxy must count as alive: {body}"
        );
        assert_eq!(body["discoveredNodes"].as_u64(), Some(1));
        assert!(body["latencyMs"].as_f64().is_some());
        assert!(body["errorCode"].is_null());
        assert_eq!(body["nodes"][0]["ok"].as_bool(), Some(true));

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn proxy_probe_502_response_keeps_node_available() {
        let db_path = temp_db_path("proxy-probe-502-available");
        let db_str = db_path.to_string_lossy().to_string();
        let upstream_addr = spawn_forward_proxy_probe_upstream().await;
        let fake_proxy_addr = spawn_fake_forward_proxy(StatusCode::BAD_GATEWAY).await;
        let upstream = format!("http://{}/mcp", upstream_addr);
        let usage_base = format!("http://{}", upstream_addr);
        let proxy =
            TavilyProxy::with_endpoint::<Vec<String>, String>(Vec::new(), &upstream, &db_str)
                .await
                .expect("create proxy");
        let addr = spawn_admin_forward_proxy_server(proxy, usage_base, true).await;

        let client = Client::new();
        let saved = client
            .put(format!("http://{addr}/api/settings/forward-proxy"))
            .json(&serde_json::json!({
                "proxyUrls": [format!("http://{}", fake_proxy_addr)],
                "subscriptionUrls": [],
                "subscriptionUpdateIntervalSecs": 3600,
                "insertDirect": false,
            }))
            .send()
            .await
            .expect("save forward proxy settings");
        assert_eq!(saved.status(), StatusCode::OK);

        let response = client
            .get(format!("http://{addr}/api/settings/forward-proxy"))
            .send()
            .await
            .expect("get forward proxy settings");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("decode forward proxy settings");
        let nodes = body["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 1, "expected only the manual node: {body}");
        let expected_endpoint_url = format!("http://{fake_proxy_addr}/");
        assert_eq!(
            nodes[0]["endpointUrl"].as_str(),
            Some(expected_endpoint_url.as_str())
        );
        assert_eq!(
            nodes[0]["available"].as_bool(),
            Some(true),
            "a 502 through the proxy must not take the node out of rotation: {body}"
        );
        assert!(nodes[0]["lastError"].is_null());

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn proxy_probe_transport_timeout_marks_proxy_dead() {
        let db_path = temp_db_path("proxy-probe-timeout");
        let db_str = db_path.to_string_lossy().to_string();
        let upstream_addr = spawn_forward_proxy_probe_upstream().await;
        let stalled_proxy_addr = spawn_stalled_forward_proxy().await;
        let upstream = format!("http://{}/mcp", upstream_addr);
        let usage_base = format!("http://{}", upstream_addr);
        let proxy =
            TavilyProxy::with_endpoint::<Vec<String>, String>(Vec::new(), &upstream, &db_str)
                .await
                .expect("create proxy");
        let addr = spawn_admin_forward_proxy_server(proxy, usage_base, true).await;

        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("build validation client");
        let response = client
            .post(format!("http://{addr}/api/settings/forward-proxy/validate"))
            .json(&serde_json::json!({
                "kind": "proxyUrl",
                "value": format!("http://{}", stalled_proxy_addr),
            }))
            .send()
            .await
            .expect("validate stalled proxy");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("decode validation");
        assert_eq!(
            body["ok"].as_bool(),
            Some(false),
            "a transport timeout must still count as dead: {body}"
        );
        assert!(body["errorCode"].as_str().is_some());
        assert_eq!(body["nodes"][0]["ok"].as_bool(), Some(false));
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("timed out"),
            "expected timeout context, got: {body}"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn proxy_probe_env_probe_url_override_and_invalid_fallback() {
        let default_probe = "http://example.com/";
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_probe_url(None).as_str(),
            default_probe
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_probe_url(Some("   ")).as_str(),
            default_probe
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_probe_url(Some(
                "http://127.0.0.1:9/health"
            ))
            .as_str(),
            "http://127.0.0.1:9/health"
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_probe_url(Some("not a url")).as_str(),
            default_probe
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_probe_url(Some(
                "ftp://example.com/"
            ))
            .as_str(),
            default_probe
        );
    }

    #[test]
    fn proxy_probe_env_timeout_override_clamped() {
        let default_secs = 5;
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(None),
            default_secs
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(Some("   ")),
            default_secs
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(Some("7")),
            7
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(Some("1")),
            2
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(Some("600")),
            60
        );
        assert_eq!(
            tavily_hikari::ForwardProxySettings::resolve_validation_timeout_secs(Some("garbage")),
            default_secs
        );
    }
