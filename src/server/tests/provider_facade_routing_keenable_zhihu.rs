//! Plan keenable-integration todo 6 bin coverage: the Keenable/Zhihu REST
//! route table, MCP ingress registration, request-kind catalog wiring, and
//! the config surface. The executor bodies stay the typed todo-2 boundaries
//! until todos 7-10 land, so the full-server cases pin those boundary
//! responses (typed 500/501, ZERO upstream calls) instead of pretending a
//! live facade — never a silent success and never an accidental dial of a
//! production endpoint.

use super::core_support_and_parsing::{connect_sqlite_test_pool, temp_db_path};
use super::*;
use tavily_hikari::providers::{self, ProviderId};

/// The four todo-6 REST route rows, pinned field by field against the table
/// the server registers (misleading-success guard: the resolved provider,
/// kind path, methods, upstream path, credit op, and operation label are
/// each asserted, not just a 200/404).
#[test]
fn keenable_zhihu_rest_routes_resolve_with_exact_table_fields() {
    for (
        path,
        kind_path,
        provider,
        allow_get,
        allow_post,
        upstream_path,
        credits_op,
        upstream_operation,
    ) in [
        (
            "/api/keenable/v1/search",
            "/api/keenable/search",
            ProviderId::Keenable,
            false,
            true,
            "/v1/search",
            "search",
            "keenable_http_search",
        ),
        (
            "/api/keenable/v1/fetch",
            "/api/keenable/fetch",
            ProviderId::Keenable,
            true,
            false,
            "/v1/fetch",
            "fetch",
            "keenable_http_fetch",
        ),
        (
            "/api/zhihu/api/v1/content/zhihu_search",
            "/api/zhihu/zhihu_search",
            ProviderId::Zhihu,
            true,
            false,
            "/api/v1/content/zhihu_search",
            "zhihu_search",
            "zhihu_http_zhihu_search",
        ),
        (
            "/api/zhihu/api/v1/content/global_search",
            "/api/zhihu/global_search",
            ProviderId::Zhihu,
            true,
            false,
            "/api/v1/content/global_search",
            "global_search",
            "zhihu_http_global_search",
        ),
    ] {
        let route = provider_facade::resolve_provider_rest_route(path)
            .unwrap_or_else(|| panic!("route {path} must be registered"));
        assert_eq!(
            provider_facade::resolve_provider_for_path(path),
            Some(provider),
            "QA happy: {path} resolves to {provider}"
        );
        assert_eq!(route.kind_path, kind_path, "{path} kind path");
        assert_eq!(route.provider, provider, "{path} provider");
        assert_eq!(route.allow_get, allow_get, "{path} GET column");
        assert_eq!(route.allow_post, allow_post, "{path} POST column");
        assert_eq!(route.upstream_path, upstream_path, "{path} upstream path");
        assert_eq!(route.credits_op, credits_op, "{path} credit op");
        assert_eq!(route.upstream_operation, upstream_operation, "{path} op");
        // The row label must equal the lib-side registry vocabulary (todo 27
        // ratifies it into the closed-enum SPEC).
        assert_eq!(
            tavily_hikari::provider_rest_upstream_operation(provider, upstream_path),
            Some(upstream_operation),
            "{path} upstream_operation registry vocabulary"
        );
        // Reserved credits come from the committed todo-2 ProviderSpec table.
        let spec = providers::spec_for(provider);
        assert_eq!(
            (spec.credits)(credits_op),
            1,
            "{path} reserved credit from the spec table"
        );
    }

    // Unknown subpaths of the provider prefixes resolve to None: they are NOT
    // routes, so only the registered MCP `/*path` reject handlers can answer
    // them (never the SPA fallback, never a stray facade).
    for unknown in [
        "/api/keenable/v1/other",
        "/api/keenable",
        "/api/zhihu/api/v1/content/hot_list",
        "/api/zhihu",
    ] {
        assert_eq!(
            provider_facade::resolve_provider_for_path(unknown),
            None,
            "{unknown} must not resolve to a provider"
        );
    }
}

/// The two todo-6 MCP ingresses: ingress resolution, the pinned lib-side
/// ingress path, and the subpath prefix that feeds the reject handlers.
#[test]
fn keenable_zhihu_mcp_ingresses_resolve_and_pin_paths() {
    assert_eq!(
        provider_facade::resolve_provider_mcp_route("/keenable/mcp"),
        Some(ProviderId::Keenable)
    );
    assert_eq!(
        provider_facade::resolve_provider_mcp_route("/zhihu/mcp"),
        Some(ProviderId::Zhihu)
    );
    assert_eq!(
        provider_facade::resolve_provider_mcp_route("/mcp"),
        None,
        "the Tavily ingress keeps its pinned registration"
    );
    // Subpaths of a provider ingress still resolve to that provider — that is
    // exactly what routes them into the typed `/*path` reject handlers.
    assert_eq!(
        provider_facade::resolve_provider_for_path("/keenable/mcp/extra"),
        Some(ProviderId::Keenable)
    );
    assert_eq!(
        provider_facade::resolve_provider_for_path("/zhihu/mcp/extra"),
        Some(ProviderId::Zhihu)
    );
    // Ingress-path pins (bin-side resolver): the paths the dispatch boundary
    // and the SQL surfaces rely on can never drift from the routing table
    // (the lib-side `provider_mcp_ingress_path` arm is pinned in
    // src/tests/provider_failure_classification.rs).
    assert_eq!(
        provider_facade::resolve_provider_mcp_ingress(ProviderId::Keenable),
        "/keenable/mcp"
    );
    assert_eq!(
        provider_facade::resolve_provider_mcp_ingress(ProviderId::Zhihu),
        "/zhihu/mcp"
    );
}

/// The eight todo-6 request kinds: BOTH the registered paths and the canonical
/// kind paths classify to the kinds, the kinds are billable, and the MCP tool
/// names route through the provider tool table (stale-state guard: a kind that
/// silently bucketed as Unknown would fail here).
#[test]
fn keenable_zhihu_request_kinds_classify_with_billable_groups() {
    for (path, key, label) in [
        (
            "/api/keenable/v1/search",
            "api:keenable/search",
            "API | keenable/search",
        ),
        (
            "/api/keenable/search",
            "api:keenable/search",
            "API | keenable/search",
        ),
        (
            "/api/keenable/v1/fetch",
            "api:keenable/fetch",
            "API | keenable/fetch",
        ),
        (
            "/api/keenable/fetch",
            "api:keenable/fetch",
            "API | keenable/fetch",
        ),
        (
            "/api/zhihu/api/v1/content/zhihu_search",
            "api:zhihu/zhihu_search",
            "API | zhihu/zhihu_search",
        ),
        (
            "/api/zhihu/zhihu_search",
            "api:zhihu/zhihu_search",
            "API | zhihu/zhihu_search",
        ),
        (
            "/api/zhihu/api/v1/content/global_search",
            "api:zhihu/global_search",
            "API | zhihu/global_search",
        ),
        (
            "/api/zhihu/global_search",
            "api:zhihu/global_search",
            "API | zhihu/global_search",
        ),
    ] {
        let kind = tavily_hikari::classify_token_request_kind(path, None);
        assert_eq!(kind.key, key, "path {path}");
        assert_eq!(kind.label, label, "path {path}");
        assert_eq!(
            tavily_hikari::token_request_kind_billing_group(key),
            "billable",
            "{key} must be billable, never a free ride"
        );
        assert!(
            tavily_hikari::is_canonical_request_kind_key(key),
            "{key} must be a canonical kind"
        );
    }

    // MCP tools/call bodies classify to the provider tool kinds on both new
    // ingresses; the repo-pinned Tavily `/mcp` classification stays untouched.
    let tools_call = |tool: &str| {
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": {} }
        }))
        .expect("tools/call body serializes")
    };
    for (path, tool, key) in [
        (
            "/keenable/mcp",
            "search_web_pages",
            "mcp:keenable/search_web_pages",
        ),
        (
            "/keenable/mcp",
            "fetch_page_content",
            "mcp:keenable/fetch_page_content",
        ),
        ("/zhihu/mcp", "zhihu_search", "mcp:zhihu/zhihu_search"),
        ("/zhihu/mcp", "zhihu_global_search", "mcp:zhihu/global_search"),
    ] {
        let kind = tavily_hikari::classify_token_request_kind(path, Some(&tools_call(tool)));
        assert_eq!(kind.key, key, "{path} tool {tool}");
        assert_eq!(
            tavily_hikari::token_request_kind_billing_group(key),
            "billable",
            "{key} must be billable"
        );
    }
    assert_eq!(
        tavily_hikari::classify_token_request_kind("/mcp", Some(&tools_call("tavily-search"))).key,
        "mcp:search",
        "the Tavily /mcp classification stays byte-identical"
    );

    // MCP subpaths keep the shared unsupported-path kind (the reject handler
    // records it, never the SPA fallback).
    let kind = tavily_hikari::classify_token_request_kind("/zhihu/mcp/extra", None);
    assert_eq!(kind.key, "mcp:unsupported-path");
    assert_eq!(kind.detail.as_deref(), Some("/zhihu/mcp/extra"));
}

/// One upstream attempt path the mock captured (stub-only rule: the harness
/// must be able to prove ZERO upstream calls for every boundary case).
async fn spawn_capturing_stub_upstream() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let attempts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let attempts_for_app = attempts.clone();
    let app = Router::new().fallback(move |req: Request<Body>| {
        let attempts = attempts_for_app.clone();
        async move {
            let (parts, body) = req.into_parts();
            let _ = body::to_bytes(body, BODY_LIMIT).await.unwrap_or_default();
            attempts
                .lock()
                .expect("attempts lock")
                .push(parts.uri.path().to_string());
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"stub":"ok"}"#))
                .expect("stub response")
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service()).await.unwrap();
    });
    (addr, attempts)
}

struct KeenableZhihuHarness {
    addr: SocketAddr,
    upstream_attempts: Arc<Mutex<Vec<String>>>,
    token: String,
    db_str: String,
    db_path: PathBuf,
}

impl Drop for KeenableZhihuHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.db_path);
    }
}

/// Full-server harness on the REAL provider facade router with stub-only
/// upstreams (127.0.0.1 mocks; the stub-only rule is a repo incident rule).
async fn spawn_keenable_zhihu_harness(prefix: &str) -> KeenableZhihuHarness {
    let db_path = temp_db_path(prefix);
    let db_str = db_path.to_string_lossy().to_string();
    let (rest_addr, upstream_attempts) = spawn_capturing_stub_upstream().await;
    let providers = tavily_hikari::ProviderBootstrap {
        serpapi_upstream: format!("http://{rest_addr}"),
        serpapi_mcp_upstream: format!("http://{rest_addr}/gw/mcp"),
        exa_upstream: format!("http://{rest_addr}"),
        brave_upstream: format!("http://{rest_addr}"),
        serpapi_keys: vec!["mock-serpapi-key".to_string()],
        exa_keys: vec!["mock-exa-key".to_string()],
        brave_keys: vec!["mock-brave-key".to_string()],
        keenable_upstream: format!("http://{rest_addr}"),
        keenable_mcp_upstream: format!("http://{rest_addr}/gw/mcp"),
        zhihu_upstream: format!("http://{rest_addr}"),
        keenable_keys: vec!["mock-keenable-key".to_string()],
        zhihu_keys: vec!["mock-zhihu-key".to_string()],
    };
    let proxy = TavilyProxy::with_providers_in_ha_mode(
        vec!["tvly-keenable-zhihu-routing-key".to_string()],
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
        usage_base: format!("http://{rest_addr}"),
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
    KeenableZhihuHarness {
        addr,
        upstream_attempts,
        token: access_token.token,
        db_str,
        db_path,
    }
}

fn upstream_attempt_count(harness: &KeenableZhihuHarness) -> usize {
    harness
        .upstream_attempts
        .lock()
        .expect("attempts lock")
        .len()
}

/// QA scenario (failure): a provider MCP subpath answers 404 `text/plain`
/// from the registered reject handler — never the SPA redirect — and leaves a
/// provider-tagged `mcp_path_404` request log with the kind surface intact.
#[tokio::test]
async fn zhihu_and_keenable_mcp_subpaths_404_text_plain_never_spa_redirect() {
    let harness = spawn_keenable_zhihu_harness("keenable-zhihu-mcp-subpath").await;
    let client = Client::new();

    for (path, provider) in [
        ("/zhihu/mcp/extra", "zhihu"),
        ("/keenable/mcp/extra", "keenable"),
    ] {
        let resp = client
            .get(format!("http://{}{path}", harness.addr))
            .header("authorization", format!("Bearer {}", harness.token))
            // An HTML-accepting client: the reject route must still win over
            // any SPA 302 fallback (M16 mirror for the todo-6 ingresses).
            .header("accept", "text/html,application/xhtml+xml")
            .send()
            .await
            .expect("mcp subpath request");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path} provider={provider}");
        assert!(
            resp.headers().get("location").is_none(),
            "{path} must never redirect to the SPA 404 landing"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8"),
            "{path} content type"
        );
        assert_eq!(resp.text().await.expect("body"), "Not Found", "{path} body");
    }

    // Zero upstream calls: the reject handler never reaches an upstream.
    assert_eq!(upstream_attempt_count(&harness), 0);

    // Provider-tagged mcp_path_404 request logs (M16), one per subpath.
    let pool = connect_sqlite_test_pool(&harness.db_str).await;
    let rows = sqlx::query(
        "SELECT provider, failure_kind, result_status, path, request_kind_key \
         FROM request_logs ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("request logs");
    let expected = [
        ("keenable", "/keenable/mcp/extra", "mcp:unsupported-path"),
        ("zhihu", "/zhihu/mcp/extra", "mcp:unsupported-path"),
    ];
    assert_eq!(rows.len(), expected.len(), "one reject row per subpath");
    // Key by path: `ORDER BY id` follows insertion order, so it must not decide
    // which provider tags which subpath row.
    let mut by_path: std::collections::HashMap<String, _> = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("path"),
                (
                    row.get::<Option<String>, _>("provider"),
                    row.get::<Option<String>, _>("failure_kind"),
                    row.get::<String, _>("result_status"),
                    row.get::<Option<String>, _>("request_kind_key"),
                ),
            )
        })
        .collect();
    assert_eq!(by_path.len(), expected.len(), "paths are distinct");
    for (provider, path, kind) in expected {
        let (row_provider, failure_kind, result_status, request_kind) = by_path
            .remove(path)
            .unwrap_or_else(|| panic!("missing reject row for {path}"));
        assert_eq!(row_provider.as_deref(), Some(provider), "{path} provider");
        assert_eq!(
            failure_kind.as_deref(),
            Some("mcp_path_404"),
            "{path} failure kind"
        );
        assert_eq!(
            result_status, "error",
            "{path} reject row is an error, never a success"
        );
        assert_eq!(request_kind.as_deref(), Some(kind), "{path} request kind");
    }
    assert!(by_path.is_empty(), "no unexpected reject rows");
    pool.close().await;
}

/// Through the REAL router, both new MCP ingresses answer the typed todo-2
/// `provider_mcp_pending` boundary until todos 8/10 land — never a 200,
/// never an upstream call, never a silent success.
#[tokio::test]
async fn keenable_and_zhihu_mcp_ingresses_answer_the_typed_pending_boundary() {
    let harness = spawn_keenable_zhihu_harness("keenable-zhihu-mcp-pending").await;
    let client = Client::new();

    for path in ["/keenable/mcp", "/zhihu/mcp"] {
        let resp = client
            .post(format!("http://{}{path}", harness.addr))
            .header("authorization", format!("Bearer {}", harness.token))
            .header(CONTENT_TYPE, "application/json")
            .body(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            )
            .send()
            .await
            .expect("mcp ingress request");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{path} typed pending boundary"
        );
        let body: Value = resp.json().await.expect("json body");
        assert_eq!(
            body.get("error").and_then(Value::as_str),
            Some("provider_mcp_pending"),
            "{path} boundary body: {body}"
        );
    }

    // Zero upstream calls: the boundary never dials the configured upstreams.
    assert_eq!(upstream_attempt_count(&harness), 0);
}

/// The four new REST routes are live through the gate stack (auth + quota
/// gates pass) but the executor stays the typed todo-2 boundary until todos
/// 7/9 wire the facade record paths: a typed 500 `proxy_error`, ZERO upstream
/// calls, an error token-log row carrying the new kind, and never a billed
/// credit.
#[tokio::test]
async fn keenable_zhihu_rest_routes_answer_the_typed_executor_boundary() {
    let harness = spawn_keenable_zhihu_harness("keenable-zhihu-rest-boundary").await;
    let client = Client::new();

    let search = client
        .post(format!("http://{}/api/keenable/v1/search", harness.addr))
        .header("authorization", format!("Bearer {}", harness.token))
        .header(CONTENT_TYPE, "application/json")
        .body(r#"{"query":"routing probe","max_results":3}"#)
        .send()
        .await
        .expect("keenable search request");
    assert_eq!(search.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: Value = search.json().await.expect("json body");
    assert_eq!(body.get("error").and_then(Value::as_str), Some("proxy_error"));

    let zhihu = client
        .get(format!(
            "http://{}/api/zhihu/api/v1/content/zhihu_search?Query=AI&Count=3",
            harness.addr
        ))
        .header("authorization", format!("Bearer {}", harness.token))
        .send()
        .await
        .expect("zhihu search request");
    assert_eq!(zhihu.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body: Value = zhihu.json().await.expect("json body");
    assert_eq!(body.get("error").and_then(Value::as_str), Some("proxy_error"));

    // Zero upstream calls: no facade execution exists before todos 7/9, so
    // nothing may ever dial the configured upstreams (stub-only rule).
    assert_eq!(upstream_attempt_count(&harness), 0);

    // The error token-log rows carry the NEW kinds (misleading-success guard:
    // the kind surface is asserted, not just the status code).
    let pool = connect_sqlite_test_pool(&harness.db_str).await;
    let rows = sqlx::query(
        "SELECT request_kind_key, result_status, business_credits \
         FROM auth_token_logs ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("token logs");
    let expected = [
        ("api:keenable/search", "error"),
        ("api:zhihu/zhihu_search", "error"),
    ];
    assert_eq!(rows.len(), expected.len(), "one error row per request");
    for (row, (kind, status)) in rows.iter().zip(expected) {
        assert_eq!(
            row.get::<Option<String>, _>("request_kind_key").as_deref(),
            Some(kind)
        );
        assert_eq!(row.get::<String, _>("result_status"), status);
        assert!(
            !matches!(row.get::<Option<i64>, _>("business_credits"), Some(n) if n > 0),
            "the boundary never bills a credit"
        );
    }
    pool.close().await;
}

/// Config surface (todo 6): the Keenable/Zhihu upstreams and key lists
/// bootstrap provider-scoped rows through the shared sync seam, and an absent
/// env list leaves that provider's rows untouched.
#[tokio::test]
async fn keenable_zhihu_bootstrap_stamps_provider_scoped_key_rows() {
    let db_path = temp_db_path("keenable-zhihu-bootstrap");
    let db_str = db_path.to_string_lossy().to_string();
    let (stub_addr, _attempts) = spawn_capturing_stub_upstream().await;
    let providers = tavily_hikari::ProviderBootstrap {
        keenable_upstream: format!("http://{stub_addr}"),
        keenable_mcp_upstream: format!("http://{stub_addr}/gw/mcp"),
        zhihu_upstream: format!("http://{stub_addr}"),
        keenable_keys: vec![" ken-a ".to_string(), "ken-b".to_string()],
        zhihu_keys: vec!["zh-a".to_string()],
        ..tavily_hikari::ProviderBootstrap::default()
    };
    let _proxy = TavilyProxy::with_providers_in_ha_mode(
        Vec::<String>::new(),
        &format!("http://{stub_addr}/mcp"),
        &db_str,
        tavily_hikari::TavilyProxyOptions::from_database_path(&db_str),
        providers,
        tavily_hikari::HaMode::Single,
    )
    .await
    .expect("proxy created");

    let pool = connect_sqlite_test_pool(&db_str).await;
    async fn provider_of_key(pool: &sqlx::SqlitePool, key: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT provider FROM api_keys WHERE api_key = ?")
            .bind(key)
            .fetch_one(pool)
            .await
            .expect("api key row")
    }
    // Keys are trimmed, empties dropped, and stamped per provider. The
    // provider-scoped re-sync matrix (a Keenable re-sync never touches Zhihu
    // rows) is pinned lib-side in src/tests/provider_config_bootstrap.rs.
    assert_eq!(provider_of_key(&pool, "ken-a").await, "keenable");
    assert_eq!(provider_of_key(&pool, "ken-b").await, "keenable");
    assert_eq!(provider_of_key(&pool, "zh-a").await, "zhihu");
    pool.close().await;
}
