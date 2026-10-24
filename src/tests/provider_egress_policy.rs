//! Provider-aware egress policy regression battery (2026-10-22 Brave audit).
//!
//! The deployment LAN cannot reach `api.search.brave.com` directly (poisoned
//! DNS, blocked TCP), so only forward-proxy egress works. When every proxy node
//! was exiled the plan silently fell back to `__direct__` and produced 340
//! `transport_send_error` rows that read as network trouble.
//!
//! Pinned here: the plan never carries a direct candidate for a
//! `ProxyRequired` provider while proxies are configured, and the send path
//! fails fast with a diagnosable error instead of burning the doomed attempt.
//! Tavily keeps its historical direct fallback byte-for-byte.
//!
//! One test covers all phases because building a `TavilyProxy` runs the full
//! SQLite schema bootstrap, which costs ~40 s per instance.

use super::*;
use crate::providers::{EgressPolicy, ProviderId};

fn proxy_settings(proxy_urls: Vec<String>) -> ForwardProxySettings {
    ForwardProxySettings {
        proxy_urls,
        subscription_urls: Vec::new(),
        subscription_update_interval_secs: 3600,
        insert_direct: true,
        egress_socks5_enabled: false,
        egress_socks5_url: String::new(),
    }
    .normalized()
}

async fn seed_provider_key(proxy: &TavilyProxy, provider: ProviderId, secret: &str) -> String {
    proxy
        .key_store
        .sync_keys(provider, &[secret.to_string()])
        .await
        .expect("seed provider key");
    sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ? LIMIT 1")
        .bind(secret)
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("seeded key id")
}

async fn direct_attempt_count(proxy: &TavilyProxy) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM forward_proxy_attempts WHERE proxy_key = '__direct__'")
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("count direct attempts")
}

async fn insert_explicit_empty_affinity_marker(proxy: &TavilyProxy, key_id: &str) {
    sqlx::query(
        "INSERT INTO forward_proxy_key_affinity (key_id, primary_proxy_key, secondary_proxy_key, updated_at) \
         VALUES (?, NULL, NULL, 0)",
    )
    .bind(key_id)
    .execute(&proxy.key_store.pool)
    .await
    .expect("insert explicit empty affinity marker");
}

async fn apply_proxy_settings(proxy: &TavilyProxy, proxy_urls: Vec<String>) {
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(proxy_urls));
}

/// A CLOSED loopback port: the plan/dial paths must stay inside this machine.
/// A public address here would make the suite depend on the internet.
fn configured_proxy() -> Vec<String> {
    vec!["http://127.0.0.1:9".to_string()]
}

#[tokio::test]
async fn provider_egress_policy_gates_brave_and_preserves_direct_fallbacks() {
    let db_path = temp_db_path("provider-egress-policy");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    let brave_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-a").await;
    let tavily_id = seed_provider_key(&proxy, ProviderId::Tavily, "tvly-egress-a").await;
    insert_explicit_empty_affinity_marker(&proxy, &brave_id).await;
    insert_explicit_empty_affinity_marker(&proxy, &tavily_id).await;

    // An explicit empty affinity marker is what the legacy builder answered
    // with a direct-only plan; it is exactly the state the gate must veto.
    apply_proxy_settings(&proxy, configured_proxy()).await;

    // The plan builder is provider-agnostic on purpose: it lists egress
    // candidates for a KEY, and only the send choke point knows the provider.
    // Two branches must hold, and the first one is what makes the strip safe:
    // a plan that still carries __direct__ must send the proxy candidate only,
    // and its failure must stay the proxy's own transport error.
    let brave_plan = proxy
        .build_proxy_attempt_plan(&brave_id)
        .await
        .expect("build brave plan");
    let plan_err = proxy
        .send_with_forward_proxy_plan(
            ProviderId::Brave,
            Some(&brave_id),
            "web",
            brave_plan,
            |client| client.get("http://127.0.0.1:9/"),
        )
        .await
        .expect_err("a dead proxy candidate must still fail the request");
    assert!(
        matches!(plan_err, ProxyError::Http(_)),
        "a proxy that was actually dialled reports its own transport error, got {plan_err:?}"
    );
    assert_eq!(
        direct_attempt_count(&proxy).await,
        0,
        "a proxy-required provider must never record a direct attempt while a proxy is configured"
    );

    let err = proxy
        .send_with_forward_proxy_plan(
            ProviderId::Brave,
            Some(&brave_id),
            "web",
            Vec::new(),
            |client| client.get("http://127.0.0.1:9/"),
        )
        .await
        .expect_err("an empty proxy plan must not silently become a direct Brave attempt");
    assert_eq!(
        err.to_string(),
        "no forward-proxy egress available for brave: 1 configured node(s), none selectable",
        "the error must name the provider and the exile, not a transport failure"
    );

    apply_proxy_settings(&proxy, Vec::new()).await;
    let stale_plan = proxy
        .build_proxy_attempt_plan(&brave_id)
        .await
        .expect("proxy-less plan");
    assert!(
        stale_plan
            .iter()
            .any(|candidate| candidate.key == forward_proxy::FORWARD_PROXY_DIRECT_KEY),
        "proxy-less deployments must still plan the direct egress"
    );
    apply_proxy_settings(&proxy, configured_proxy()).await;
    let stale_err = proxy
        .send_with_forward_proxy_plan(
            ProviderId::Brave,
            Some(&brave_id),
            "web",
            stale_plan,
            |client| client.get("http://127.0.0.1:9/"),
        )
        .await
        .expect_err("the stale direct candidate must be stripped before send");
    assert!(
        matches!(stale_err, ProxyError::ForwardProxyEgressUnavailable { .. }),
        "got {stale_err:?}"
    );

    assert_eq!(
        crate::providers::spec_for(ProviderId::Brave).egress,
        EgressPolicy::ProxyRequired
    );
    assert_eq!(
        crate::providers::spec_for(ProviderId::Tavily).egress,
        EgressPolicy::DirectAllowed
    );
    let (brave_direct, brave_nodes) = proxy.direct_egress_decision(ProviderId::Brave).await;
    assert!(
        !brave_direct,
        "brave must not use the direct egress while a proxy is configured"
    );
    assert_eq!(brave_nodes, 1);
    let (tavily_direct, _) = proxy.direct_egress_decision(ProviderId::Tavily).await;
    assert!(tavily_direct, "tavily keeps its historical direct fallback");

    let tavily_outcome = proxy
        .send_with_forward_proxy_plan(
            ProviderId::Tavily,
            Some(&tavily_id),
            "search",
            Vec::new(),
            |client| client.get("http://127.0.0.1:9/"),
        )
        .await;
    if let Err(err) = &tavily_outcome {
        assert!(
            !matches!(err, ProxyError::ForwardProxyEgressUnavailable { .. }),
            "a direct-allowed provider must never be gated by the proxy egress policy, got {err:?}"
        );
    }
    assert_eq!(
        direct_attempt_count(&proxy).await,
        1,
        "only the direct-allowed provider may record a direct attempt"
    );

    apply_proxy_settings(&proxy, Vec::new()).await;
    let proxy_less_plan = proxy
        .build_proxy_attempt_plan(&brave_id)
        .await
        .expect("proxy-less brave plan");
    assert_eq!(
        proxy_less_plan
            .iter()
            .map(|candidate| candidate.key.as_str())
            .collect::<Vec<_>>(),
        vec![forward_proxy::FORWARD_PROXY_DIRECT_KEY],
        "proxy-less deployments must keep the direct egress for every provider"
    );
    let brave_proxy_less = proxy
        .send_with_forward_proxy_plan(
            ProviderId::Brave,
            Some(&brave_id),
            "web",
            Vec::new(),
            |client| client.get("http://127.0.0.1:9/"),
        )
        .await;
    if let Err(err) = &brave_proxy_less {
        assert!(
            !matches!(err, ProxyError::ForwardProxyEgressUnavailable { .. }),
            "a proxy-less install has no alternative egress; direct must stay allowed, got {err:?}"
        );
    }
    assert_eq!(
        direct_attempt_count(&proxy).await,
        2,
        "the direct egress must still carry a proxy-less Brave deployment"
    );

    let _ = std::fs::remove_file(db_path);
}

#[test]
fn transport_failure_kinds_come_from_the_error_type_not_its_wording() {
    // The store derives failure_kind from the error's Display text as a
    // fallback. Rows that still hold the typed error must not depend on that
    // prose: reword the Display and the diagnosis has to stay identical.
    assert_eq!(
        failure_kind_from_proxy_error(&ProxyError::ForwardProxyEgressUnavailable {
            provider: "brave",
            configured_nodes: 2,
        }),
        Some("forward_proxy_egress_unavailable")
    );

    // A REAL connect failure: an address that can never be dialled. This is
    // the shape every transport row in the audit had, so it is what the mapping
    // must recognise - without depending on the error's wording.
    let connect_err = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            reqwest::Client::new()
                .get("http://127.0.0.1:9/")
                .send()
                .await
                .expect_err("closed port must fail")
        });
    assert!(connect_err.is_connect(), "precondition: {connect_err:?}");
    assert_eq!(
        failure_kind_from_proxy_error(&ProxyError::Http(connect_err)),
        Some("transport_send_error")
    );

    // A non-transport variant keeps falling through to the store classifier:
    // stamping it here would freeze a diagnosis the classifier derives from
    // status and body instead.
    assert_eq!(
        failure_kind_from_proxy_error(&ProxyError::NoAvailableKeys),
        None,
        "variants whose kind is decided by status or body must keep the fallback"
    );
}
