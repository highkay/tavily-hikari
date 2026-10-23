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

fn configured_proxy() -> Vec<String> {
    vec!["http://18.183.246.69:8080".to_string()]
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

    let brave_plan = proxy
        .build_proxy_attempt_plan(&brave_id)
        .await
        .expect("build brave plan");
    assert!(
        brave_plan
            .iter()
            .all(|candidate| candidate.key != forward_proxy::FORWARD_PROXY_DIRECT_KEY),
        "a proxy-required provider must never plan __direct__ while a proxy is configured: {:?}",
        brave_plan
            .iter()
            .map(|c| c.key.as_str())
            .collect::<Vec<_>>()
    );

    let err = proxy
        .send_with_forward_proxy_plan(&brave_id, Some(&brave_id), "web", Vec::new(), |client| {
            client.get("http://127.0.0.1:9/")
        })
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
        .send_with_forward_proxy_plan(&brave_id, Some(&brave_id), "web", stale_plan, |client| {
            client.get("http://127.0.0.1:9/")
        })
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
    assert!(
        !proxy
            .direct_egress_permitted_for_subject(&brave_id)
            .await
            .expect("brave permission")
    );
    assert!(
        proxy
            .direct_egress_permitted_for_subject(&tavily_id)
            .await
            .expect("tavily permission")
    );

    let tavily_outcome = proxy
        .send_with_forward_proxy_plan(
            &tavily_id,
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
        .send_with_forward_proxy_plan(&brave_id, Some(&brave_id), "web", Vec::new(), |client| {
            client.get("http://127.0.0.1:9/")
        })
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
