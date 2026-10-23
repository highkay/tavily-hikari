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

/// QA happy path for the incident: a Brave key with an explicit empty affinity
/// marker must produce a plan WITHOUT `__direct__` while a proxy is configured.
#[tokio::test]
async fn brave_plan_excludes_direct_while_a_proxy_is_configured() {
    let db_path = temp_db_path("egress-brave-plan-no-direct");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(vec![
            "http://18.183.246.69:8080".to_string(),
        ]));

    let key_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-plan-a").await;
    sqlx::query(
        "INSERT INTO forward_proxy_key_affinity (key_id, primary_proxy_key, secondary_proxy_key, updated_at) \
         VALUES (?, NULL, NULL, 0)",
    )
    .bind(&key_id)
    .execute(&proxy.key_store.pool)
    .await
    .expect("insert explicit empty affinity marker");

    let plan = proxy
        .build_proxy_attempt_plan(&key_id)
        .await
        .expect("build brave plan");
    assert!(
        plan.iter()
            .all(|candidate| candidate.key != forward_proxy::FORWARD_PROXY_DIRECT_KEY),
        "a proxy-required provider must never plan __direct__ while a proxy is configured: {:?}",
        plan.iter().map(|c| c.key.as_str()).collect::<Vec<_>>()
    );

    let _ = std::fs::remove_file(db_path);
}

/// Negative control: a proxy-less deployment has no alternative egress, so the
/// same Brave key still gets the direct candidate.
#[tokio::test]
async fn brave_plan_keeps_direct_when_no_proxy_is_configured() {
    let db_path = temp_db_path("egress-brave-plan-no-proxy");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(Vec::new()));

    let key_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-plan-b").await;
    sqlx::query(
        "INSERT INTO forward_proxy_key_affinity (key_id, primary_proxy_key, secondary_proxy_key, updated_at) \
         VALUES (?, NULL, NULL, 0)",
    )
    .bind(&key_id)
    .execute(&proxy.key_store.pool)
    .await
    .expect("insert explicit empty affinity marker");

    let plan = proxy
        .build_proxy_attempt_plan(&key_id)
        .await
        .expect("build brave plan");
    assert_eq!(
        plan.iter()
            .map(|candidate| candidate.key.as_str())
            .collect::<Vec<_>>(),
        vec![forward_proxy::FORWARD_PROXY_DIRECT_KEY],
        "proxy-less deployments must keep the direct egress for every provider"
    );

    let _ = std::fs::remove_file(db_path);
}

/// The incident's failure mode at the send seam: an empty plan must NOT become
/// a direct attempt for Brave; it must fail before any upstream socket exists.
#[tokio::test]
async fn brave_send_fails_fast_instead_of_falling_back_to_direct() {
    let db_path = temp_db_path("egress-brave-send-fail-fast");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(vec![
            "http://18.183.246.69:8080".to_string(),
        ]));
    let key_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-send-a").await;

    let err = proxy
        .send_with_forward_proxy_plan(&key_id, Some(&key_id), "web", Vec::new(), |client| {
            client.get("http://127.0.0.1:9/")
        })
        .await
        .expect_err("an empty proxy plan must not silently become a direct Brave attempt");

    assert_eq!(
        err.to_string(),
        "no forward-proxy egress available for brave: 1 configured node(s), none selectable",
        "the error must name the provider and the exile, not a transport failure"
    );
    assert!(
        matches!(
            err,
            ProxyError::ForwardProxyEgressUnavailable {
                provider: "brave",
                configured_nodes: 1
            }
        ),
        "got {err:?}"
    );

    let direct_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM forward_proxy_attempts WHERE proxy_key = '__direct__'",
    )
    .fetch_one(&proxy.key_store.pool)
    .await
    .expect("count direct attempts");
    assert_eq!(
        direct_attempts, 0,
        "the gate must prevent the direct attempt from ever being recorded"
    );

    let _ = std::fs::remove_file(db_path);
}

/// Regression guard for the other three providers: the historical direct
/// fallback must survive untouched (byte-stable rule).
#[tokio::test]
async fn tavily_send_keeps_the_historical_direct_fallback() {
    let db_path = temp_db_path("egress-tavily-direct-fallback");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(vec![
            "http://18.183.246.69:8080".to_string(),
        ]));
    let key_id = seed_provider_key(&proxy, ProviderId::Tavily, "tvly-egress-send-a").await;

    let outcome = proxy
        .send_with_forward_proxy_plan(&key_id, Some(&key_id), "search", Vec::new(), |client| {
            client.get("http://127.0.0.1:9/")
        })
        .await;

    if let Err(err) = &outcome {
        assert!(
            !matches!(err, ProxyError::ForwardProxyEgressUnavailable { .. }),
            "a direct-allowed provider must never be gated by the proxy egress policy, got {err:?}"
        );
    }

    let direct_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM forward_proxy_attempts WHERE proxy_key = '__direct__'",
    )
    .fetch_one(&proxy.key_store.pool)
    .await
    .expect("count direct attempts");
    assert_eq!(
        direct_attempts, 1,
        "a direct-allowed provider must still send and record its direct attempt"
    );

    let _ = std::fs::remove_file(db_path);
}

/// The stance comes from the provider registry, narrowed by "are any proxies
/// configured", never re-hardcoded at a call site.
#[tokio::test]
async fn stale_direct_plan_candidate_is_stripped_for_brave_at_the_send_seam() {
    let db_path = temp_db_path("egress-brave-stale-direct-plan");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    let key_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-stale-a").await;

    // Plan built while proxy-less (direct candidate is legitimate then).
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(Vec::new()));
    let stale_plan = proxy
        .build_proxy_attempt_plan(&key_id)
        .await
        .expect("proxy-less plan");

    // Proxies appear before the send: the stale direct candidate must not fire.
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(vec![
            "http://18.183.246.69:8080".to_string(),
        ]));

    let err = proxy
        .send_with_forward_proxy_plan(&key_id, Some(&key_id), "web", stale_plan, |client| {
            client.get("http://127.0.0.1:9/")
        })
        .await
        .expect_err("the stale direct candidate must be stripped before send");
    assert!(
        matches!(err, ProxyError::ForwardProxyEgressUnavailable { .. }),
        "got {err:?}"
    );

    let direct_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM forward_proxy_attempts WHERE proxy_key = '__direct__'",
    )
    .fetch_one(&proxy.key_store.pool)
    .await
    .expect("count direct attempts");
    assert_eq!(
        direct_attempts, 0,
        "no direct Brave attempt may ever be sent"
    );

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn brave_send_keeps_direct_when_no_proxy_is_configured() {
    let db_path = temp_db_path("egress-brave-send-no-proxy");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(Vec::new()));
    let key_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-send-b").await;

    let outcome = proxy
        .send_with_forward_proxy_plan(&key_id, Some(&key_id), "web", Vec::new(), |client| {
            client.get("http://127.0.0.1:9/")
        })
        .await;
    if let Err(err) = &outcome {
        assert!(
            !matches!(err, ProxyError::ForwardProxyEgressUnavailable { .. }),
            "a proxy-less install has no alternative egress; direct must stay allowed, got {err:?}"
        );
    }

    let direct_attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM forward_proxy_attempts WHERE proxy_key = '__direct__'",
    )
    .fetch_one(&proxy.key_store.pool)
    .await
    .expect("count direct attempts");
    assert_eq!(
        direct_attempts, 1,
        "the direct egress must still carry a proxy-less Brave deployment"
    );

    let _ = std::fs::remove_file(db_path);
}

#[tokio::test]
async fn egress_permission_follows_the_registry_and_configured_nodes() {
    let db_path = temp_db_path("egress-permission-matrix");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("proxy created");
    let brave_id = seed_provider_key(&proxy, ProviderId::Brave, "brv-egress-matrix-a").await;
    let tavily_id = seed_provider_key(&proxy, ProviderId::Tavily, "tvly-egress-matrix-a").await;

    // Proxy configured: Brave loses direct, Tavily keeps it.
    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(vec![
            "http://18.183.246.69:8080".to_string(),
        ]));
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

    proxy
        .forward_proxy
        .lock()
        .await
        .apply_settings(proxy_settings(Vec::new()));
    assert!(
        proxy
            .direct_egress_permitted_for_subject(&brave_id)
            .await
            .expect("brave permission without proxies")
    );

    assert_eq!(
        crate::providers::spec_for(ProviderId::Brave).egress,
        EgressPolicy::ProxyRequired
    );

    let _ = std::fs::remove_file(db_path);
}
