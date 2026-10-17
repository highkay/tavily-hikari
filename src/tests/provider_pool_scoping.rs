//! Provider-scoped key acquisition, recovery policies, and lease isolation
//! (plan todo 9 + amendments M3/M8).
//!
//! Coverage map:
//! - C3 mandatory clock-divergence test with the injectable BackendTime clock
//!   (ADR docs/adr/2026-06-15-backend-deterministic-time.md): exhausted Tavily
//!   keys recover at the UTC month start byte-identically, exhausted Exa keys
//!   NEVER recover on any calendar flip, exhausted SerpApi keys recover only
//!   at/after `quota_renews_at`, and the same run asserts no cross-provider
//!   leasing.
//! - QA happy scenario: seed 2 tavily + 2 brave keys, acquisition for Brave
//!   leases only brave key ids.
//! - QA failure scenario: the requested provider's tiers empty ->
//!   NoAvailableKeys for that provider while a Tavily acquire in the same run
//!   still succeeds. (Interpretation note: with the byte-stable 3-tier pool an
//!   exhausted-not-quarantined key stays leasable as the tier-2 fallback for
//!   EVERY provider - that preserved Tavily semantics is pinned below - so the
//!   terminal "no keys" state is exhaustion + quarantine, exactly the pool
//!   state the health machinery ends in for unbillable keys.)
//! - Exa success-restore asserted separately (RecoveryPolicy::SuccessOnly).
//! - Strict cross-key-retry acquisition (plan provider-key-429-retry todo 5):
//!   cooled/excluded keys are skipped without fallback, an exhausted key is
//!   selectable only once its recovery window expired (UtcMonth flip, SerpApi
//!   `quota_renews_at` revival, never for Exa success-only), the low-quota
//!   depletion window is honored, the token affinity candidate wins without
//!   any rebind write, and the `None` outcome leaves every marking table and
//!   the whole `api_keys` image byte-identical.

use super::*;

use crate::backend_time::BackendTime;
use crate::providers::ProviderId;

fn utc_ts(rfc3339: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("valid rfc3339 fixture timestamp")
        .with_timezone(&Utc)
}

async fn key_id(pool: &sqlx::SqlitePool, secret: &str) -> String {
    sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ?")
        .bind(secret)
        .fetch_one(pool)
        .await
        .expect("key id lookup")
}

async fn key_status(pool: &sqlx::SqlitePool, secret: &str) -> String {
    sqlx::query_scalar("SELECT status FROM api_keys WHERE api_key = ?")
        .bind(secret)
        .fetch_one(pool)
        .await
        .expect("key status lookup")
}

async fn seed_provider_keys(store: &KeyStore, provider: ProviderId, keys: &[String]) {
    store
        .sync_keys(provider, keys)
        .await
        .expect("seed provider keys");
}

/// QA happy path: a mixed pool leases ONLY the requested provider's keys.
#[tokio::test]
async fn acquire_leases_only_requested_provider_keys() {
    let db_path = temp_db_path("provider-pool-lease-isolation");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    let tavily_keys = ["tvly-pp-a", "tvly-pp-b"].map(str::to_string);
    let brave_keys = ["brv-pp-a", "brv-pp-b"].map(str::to_string);
    seed_provider_keys(&store, ProviderId::Tavily, &tavily_keys).await;
    seed_provider_keys(&store, ProviderId::Brave, &brave_keys).await;

    let brave_ids: std::collections::HashSet<String> = [
        key_id(&store.pool, "brv-pp-a").await,
        key_id(&store.pool, "brv-pp-b").await,
    ]
    .into_iter()
    .collect();
    let tavily_ids: std::collections::HashSet<String> = [
        key_id(&store.pool, "tvly-pp-a").await,
        key_id(&store.pool, "tvly-pp-b").await,
    ]
    .into_iter()
    .collect();

    // Two Brave acquisitions lease exactly the two brave keys (LRU rotation).
    let first = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect("first brave lease");
    let second = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect("second brave lease");
    assert!(brave_ids.contains(&first.id));
    assert!(brave_ids.contains(&second.id));
    assert_ne!(first.id, second.id);
    assert!(first.secret.starts_with("brv-pp-"));
    assert!(second.secret.starts_with("brv-pp-"));

    // Tavily acquisitions (new API and the legacy wrapper) lease only tavily.
    let tavily_a = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("tavily lease");
    let tavily_b = store.acquire_key().await.expect("legacy tavily lease");
    assert!(tavily_ids.contains(&tavily_a.id));
    assert!(tavily_ids.contains(&tavily_b.id));
    assert!(tavily_a.secret.starts_with("tvly-pp-"));
    assert!(tavily_b.secret.starts_with("tvly-pp-"));
}

/// QA failure path: the REQUESTED provider's tiers empty => NoAvailableKeys,
/// while a Tavily acquire in the same run still succeeds (no cross-provider
/// fallback, hard rule).
#[tokio::test]
async fn exhausted_provider_pool_yields_no_available_keys_while_tavily_succeeds() {
    let db_path = temp_db_path("provider-pool-exhausted-brave");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    let tavily_keys = ["tvly-pe-a", "tvly-pe-b"].map(str::to_string);
    let brave_keys = ["brv-pe-a", "brv-pe-b"].map(str::to_string);
    seed_provider_keys(&store, ProviderId::Tavily, &tavily_keys).await;
    seed_provider_keys(&store, ProviderId::Brave, &brave_keys).await;

    for secret in ["brv-pe-a", "brv-pe-b"] {
        assert!(
            store
                .mark_quota_exhausted(secret)
                .await
                .expect("mark brave exhausted")
        );
    }

    // Preserved 3-tier fallback (byte-stable for EVERY provider): an
    // exhausted-not-quarantined key remains leasable via the tier-2 retry
    // fallback, so "all exhausted" alone does NOT empty the provider's tiers.
    let tier2_lease = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect("exhausted brave key stays leasable as tier-2 fallback");
    assert!(tier2_lease.secret.starts_with("brv-pe-"));

    // Terminal unusable state: exhausted + quarantined (what the health
    // machinery ends in for auth-failed keys) empties every brave tier.
    for secret in ["brv-pe-a", "brv-pe-b"] {
        let id = key_id(&store.pool, secret).await;
        assert!(
            store
                .quarantine_key_by_id(
                    &id,
                    "/api/brave/res/v1/web/search",
                    "test_quarantine",
                    "test quarantine",
                    "test"
                )
                .await
                .expect("quarantine exhausted brave key")
        );
    }

    let err = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect_err("brave tiers are empty");
    assert!(matches!(err, ProxyError::NoAvailableKeys));
    assert!(
        store
            .list_mcp_session_candidate_key_ids(ProviderId::Brave)
            .await
            .expect("brave candidate listing")
            .is_empty()
    );

    // The same run: Tavily still leases normally.
    let tavily_lease = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("tavily unaffected by exhausted brave pool");
    assert!(tavily_lease.secret.starts_with("tvly-pe-"));
    let legacy_lease = store.acquire_key().await.expect("legacy tavily wrapper");
    assert!(legacy_lease.secret.starts_with("tvly-pe-"));
    assert_eq!(
        store
            .list_mcp_session_candidate_key_ids(ProviderId::Tavily)
            .await
            .expect("tavily candidate listing")
            .len(),
        2
    );
}

/// C3 (M3, MANDATORY): with the injectable BackendTime clock, a UTC month flip
/// recovers exhausted Tavily keys byte-identically and ONLY those - Exa keys
/// never recover on any calendar flip, SerpApi keys recover only at/after
/// quota_renews_at, Brave keys default to the UTC-month flip - and the same
/// run asserts no cross-provider leasing.
#[tokio::test]
async fn reset_monthly_recovers_each_provider_by_policy() {
    let db_path = temp_db_path("provider-pool-reset-monthly-c3");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-c3".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Exa, &["exa-c3".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Brave, &["brv-c3".to_string()]).await;
    seed_provider_keys(
        &store,
        ProviderId::Serpapi,
        &["srp-c3".to_string(), "srp-c3-null-renewal".to_string()],
    )
    .await;

    for secret in [
        "tvly-c3",
        "exa-c3",
        "brv-c3",
        "srp-c3",
        "srp-c3-null-renewal",
    ] {
        assert!(
            store
                .mark_quota_exhausted(secret)
                .await
                .expect("seed exhausted")
        );
    }

    // SerpApi per-key renewal date from the /account sync (plan_renewal_date).
    let renews_at = utc_ts("2026-04-10T00:00:00Z").timestamp();
    sqlx::query("UPDATE api_keys SET quota_renews_at = ? WHERE api_key = ?")
        .bind(renews_at)
        .bind("srp-c3")
        .execute(&store.pool)
        .await
        .expect("seed quota_renews_at");

    // Flip the UTC month: 2026-03-15 -> 2026-04-01T00:00:01Z.
    clock.set_now_utc(utc_ts("2026-04-01T00:00:01Z"));

    // Tavily-scoped reset recovers the Tavily key exactly as before (the
    // UtcMonth SQL plus its provider scope) and NOTHING else - this is the
    // M3 guarantee: no naive global upgrade resurrecting Exa/SerpApi keys.
    store
        .reset_monthly(ProviderId::Tavily)
        .await
        .expect("tavily reset");
    assert_eq!(key_status(&store.pool, "tvly-c3").await, "active");
    assert_eq!(key_status(&store.pool, "exa-c3").await, "exhausted");
    assert_eq!(key_status(&store.pool, "brv-c3").await, "exhausted");
    assert_eq!(key_status(&store.pool, "srp-c3").await, "exhausted");
    assert_eq!(
        key_status(&store.pool, "srp-c3-null-renewal").await,
        "exhausted"
    );

    // Exa: SuccessOnly - NO calendar flip ever recovers it (several month
    // boundaries incl. year rollover), while Tavily stays recovered.
    for boundary in [
        "2026-05-01T00:00:01Z",
        "2026-12-01T00:00:01Z",
        "2027-01-01T00:00:01Z",
    ] {
        clock.set_now_utc(utc_ts(boundary));
        store
            .reset_monthly(ProviderId::Exa)
            .await
            .expect("exa reset is a no-op");
        assert_eq!(
            key_status(&store.pool, "exa-c3").await,
            "exhausted",
            "exa must not recover on calendar boundary {boundary}"
        );
    }

    // SerpApi: RenewalDate - still exhausted strictly before quota_renews_at
    // even though two month boundaries have passed.
    clock.set_now_utc(utc_ts("2026-04-09T23:59:59Z"));
    store
        .reset_monthly(ProviderId::Serpapi)
        .await
        .expect("serpapi reset before renewal");
    assert_eq!(key_status(&store.pool, "srp-c3").await, "exhausted");

    // At exactly quota_renews_at (`<=` boundary) the key recovers; the key
    // with an unknown (NULL) renewal date stays exhausted.
    clock.set_now_utc(utc_ts("2026-04-10T00:00:00Z"));
    store
        .reset_monthly(ProviderId::Serpapi)
        .await
        .expect("serpapi reset at renewal");
    assert_eq!(key_status(&store.pool, "srp-c3").await, "active");
    assert_eq!(
        key_status(&store.pool, "srp-c3-null-renewal").await,
        "exhausted"
    );

    // Brave: documented UtcMonth default for the pool flip (the harvested
    // X-RateLimit-Reset window informs transient backoff only).
    clock.set_now_utc(utc_ts("2026-05-01T00:00:02Z"));
    store
        .reset_monthly(ProviderId::Brave)
        .await
        .expect("brave reset");
    assert_eq!(key_status(&store.pool, "brv-c3").await, "active");
    assert_eq!(key_status(&store.pool, "exa-c3").await, "exhausted");

    // Same-run no-cross-provider-leasing assertion: each provider acquires
    // only its own keys.
    clock.set_now_utc(utc_ts("2026-05-02T00:00:00Z"));
    let tavily_lease = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("tavily acquire");
    assert_eq!(tavily_lease.id, key_id(&store.pool, "tvly-c3").await);
    let exa_lease = store
        .acquire_key_for_provider(ProviderId::Exa)
        .await
        .expect("exa acquire via preserved tier-2 exhausted fallback");
    assert_eq!(exa_lease.id, key_id(&store.pool, "exa-c3").await);
    let brave_lease = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect("brave acquire");
    assert_eq!(brave_lease.id, key_id(&store.pool, "brv-c3").await);
    // SerpApi tier-1 finds the renewal-recovered key, never the NULL-renewal
    // sibling that is still exhausted.
    let serpapi_lease = store
        .acquire_key_for_provider(ProviderId::Serpapi)
        .await
        .expect("serpapi acquire");
    assert_eq!(serpapi_lease.id, key_id(&store.pool, "srp-c3").await);
}

/// Exa SuccessOnly recovery: exhausted Exa keys cross calendar boundaries
/// without flipping and recover ONLY through the success-restore path
/// (`restore_active_status`, wired for every provider via
/// reconcile_key_health). SerpApi's success-restore also applies alongside
/// its renewal-date recovery.
#[tokio::test]
async fn exhausted_success_only_keys_restore_on_success_not_calendar() {
    let db_path = temp_db_path("provider-pool-success-restore");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-06-10T08:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    let exa_keys = ["exa-sr-a", "exa-sr-b"].map(str::to_string);
    let serpapi_keys = ["srp-sr-a"].map(str::to_string);
    seed_provider_keys(&store, ProviderId::Exa, &exa_keys).await;
    seed_provider_keys(&store, ProviderId::Serpapi, &serpapi_keys).await;

    for secret in ["exa-sr-a", "exa-sr-b", "srp-sr-a"] {
        assert!(
            store
                .mark_quota_exhausted(secret)
                .await
                .expect("seed exhausted")
        );
    }

    // Calendar flips never recover success-only keys...
    for boundary in ["2026-07-01T00:00:01Z", "2026-08-01T00:00:01Z"] {
        clock.set_now_utc(utc_ts(boundary));
        store
            .reset_monthly(ProviderId::Exa)
            .await
            .expect("exa no-op");
        store
            .reset_monthly(ProviderId::Serpapi)
            .await
            .expect("serpapi reset without renewal date");
        assert_eq!(key_status(&store.pool, "exa-sr-a").await, "exhausted");
        assert_eq!(key_status(&store.pool, "exa-sr-b").await, "exhausted");
        assert_eq!(key_status(&store.pool, "srp-sr-a").await, "exhausted");
    }

    // ...but the next successful call restores them (success-restore path).
    assert!(
        store
            .restore_active_status("exa-sr-a")
            .await
            .expect("exa success restore")
    );
    assert!(
        store
            .restore_active_status("srp-sr-a")
            .await
            .expect("serpapi success restore")
    );
    assert_eq!(key_status(&store.pool, "exa-sr-a").await, "active");
    assert_eq!(key_status(&store.pool, "srp-sr-a").await, "active");
    assert_eq!(key_status(&store.pool, "exa-sr-b").await, "exhausted");

    // Re-exhausting pins SuccessOnly again: another calendar flip keeps it.
    assert!(
        store
            .mark_quota_exhausted("exa-sr-a")
            .await
            .expect("re-exhaust")
    );
    clock.set_now_utc(utc_ts("2026-09-01T00:00:01Z"));
    store
        .reset_monthly(ProviderId::Exa)
        .await
        .expect("exa no-op");
    assert_eq!(key_status(&store.pool, "exa-sr-a").await, "exhausted");
}

/// Affinity rows need no schema change because the key implies the provider:
/// every affinity/specific acquisition path rejects a foreign provider's key
/// id even when that key is perfectly healthy.
#[tokio::test]
async fn affinity_specific_acquisition_rejects_foreign_provider_keys() {
    let db_path = temp_db_path("provider-pool-affinity-foreign");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-af-a".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Brave, &["brv-af-a".to_string()]).await;
    let tavily_id = key_id(&store.pool, "tvly-af-a").await;
    let brave_id = key_id(&store.pool, "brv-af-a").await;

    // Cross-provider specific acquisition is refused (both keys are active).
    assert!(
        store
            .try_acquire_specific_key_for_provider(ProviderId::Brave, &tavily_id)
            .await
            .expect("foreign specific acquire")
            .is_none()
    );
    assert!(
        store
            .try_acquire_affinity_specific_key(ProviderId::Brave, &tavily_id)
            .await
            .expect("foreign affinity acquire")
            .is_none()
    );

    // Same-provider acquisition works, incl. the legacy Tavily wrapper.
    assert!(
        store
            .try_acquire_affinity_specific_key(ProviderId::Brave, &brave_id)
            .await
            .expect("brave affinity acquire")
            .is_some()
    );
    assert!(
        store
            .try_acquire_specific_key(&tavily_id)
            .await
            .expect("legacy tavily specific acquire")
            .is_some()
    );

    // Availability probes are provider-scoped: emptying the brave active
    // tier leaves Tavily availability intact.
    assert!(
        store
            .has_available_active_key_excluding(ProviderId::Brave, None)
            .await
            .expect("brave availability")
    );
    store
        .mark_quota_exhausted("brv-af-a")
        .await
        .expect("exhaust brave");
    assert!(
        store
            .quarantine_key_by_id(
                &brave_id,
                "/api/brave/res/v1/web/search",
                "test_quarantine",
                "test quarantine",
                "test"
            )
            .await
            .expect("quarantine brave")
    );
    assert!(
        !store
            .has_available_active_key_excluding(ProviderId::Brave, None)
            .await
            .expect("brave unavailability")
    );
    assert!(
        store
            .has_available_active_key_excluding(ProviderId::Tavily, None)
            .await
            .expect("tavily availability")
    );
}

/// Backoff-avoiding acquisition threads the provider through every fallback:
/// a scoped miss NEVER falls back to another provider's pool.
#[tokio::test]
async fn backoff_avoiding_acquire_never_crosses_providers() {
    let db_path = temp_db_path("provider-pool-backoff-scope");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-bk-a".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Brave, &["brv-bk-a".to_string()]).await;
    let brave_id = key_id(&store.pool, "brv-bk-a").await;

    let lease = store
        .acquire_key_avoiding_transient_backoff(ProviderId::Brave, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("brave backoff-avoiding acquire");
    assert_eq!(lease.id, brave_id);

    // Excluding the only brave key: the active-only fallback raises
    // NoAvailableKeys even though the tavily key is leasable - the hard
    // no-cross-provider-fallback rule.
    let err = store
        .acquire_key_avoiding_transient_backoff_excluding(
            ProviderId::Brave,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            Some(&brave_id),
        )
        .await
        .expect_err("no brave key left");
    assert!(matches!(err, ProxyError::NoAvailableKeys));

    // An armed brave backoff degrades to the full brave 3-tier pool exactly
    // like the Tavily path degrades today (byte-stable fallback semantics),
    // never to the tavily pool.
    let now = clock.now_ts();
    assert!(
        store
            .arm_api_key_transient_backoff(ApiKeyTransientBackoffArm {
                key_id: &brave_id,
                scope: HTTP_GLOBAL_BACKOFF_SCOPE,
                cooldown_until: now + 60,
                retry_after_secs: 60,
                reason_code: None,
                source_request_log_id: None,
                now,
            })
            .await
            .expect("arm brave backoff")
            .is_some()
    );
    let degraded = store
        .acquire_key_avoiding_transient_backoff(ProviderId::Brave, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("brave fallback stays in-provider");
    assert_eq!(degraded.id, brave_id);

    let tavily_lease = store
        .acquire_key_avoiding_transient_backoff(ProviderId::Tavily, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("tavily backoff-avoiding acquire");
    assert_eq!(tavily_lease.id, key_id(&store.pool, "tvly-bk-a").await);
}

/// Affinity-layer entry points (TavilyProxy) filter candidates by the
/// request's provider: legacy wrappers stay byte-identical Tavily paths and
/// the new provider variants never lease across providers.
#[tokio::test]
async fn proxy_acquire_for_provider_leases_matching_provider_only() {
    let db_path = temp_db_path("provider-pool-proxy-affinity");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let proxy = TavilyProxy::with_options_and_time(
        ["tvly-px-a".to_string()],
        DEFAULT_UPSTREAM,
        &db_str,
        TavilyProxyOptions::from_database_path(&db_str),
        backend_time.clone(),
    )
    .await
    .expect("proxy created");
    seed_provider_keys(
        &proxy.key_store,
        ProviderId::Brave,
        &["brv-px-a".to_string(), "brv-px-b".to_string()],
    )
    .await;

    let tavily_id = key_id(&proxy.key_store.pool, "tvly-px-a").await;
    let brave_ids: std::collections::HashSet<String> = [
        key_id(&proxy.key_store.pool, "brv-px-a").await,
        key_id(&proxy.key_store.pool, "brv-px-b").await,
    ]
    .into_iter()
    .collect();

    // Legacy acquire_key_for: byte-stable Tavily path despite brave rows.
    let legacy = proxy
        .acquire_key_for(None)
        .await
        .expect("legacy acquire_key_for");
    assert_eq!(legacy.id, tavily_id);

    // Provider variant: global-scheduling path leases brave keys only.
    for _ in 0..2 {
        let lease = proxy
            .acquire_key_for_provider(ProviderId::Brave, None)
            .await
            .expect("brave acquire_key_for_provider");
        assert!(brave_ids.contains(&lease.id));
    }

    // MCP-session-init entry point (None token = global scheduling).
    let selection = proxy
        .acquire_key_for_mcp_session_init_provider(ProviderId::Brave, None)
        .await
        .expect("brave mcp-session-init acquire");
    assert!(brave_ids.contains(&selection.lease.id));
    let legacy_selection = proxy
        .acquire_key_for_mcp_session_init(None)
        .await
        .expect("legacy mcp-session-init acquire");
    assert_eq!(legacy_selection.lease.id, tavily_id);

    // API-route entry point (anonymous subject path).
    let route_selection = proxy
        .acquire_key_for_api_route_provider(ProviderId::Brave, None, None)
        .await
        .expect("brave api-route acquire");
    assert!(brave_ids.contains(&route_selection.lease.id));
    let legacy_route = proxy
        .acquire_key_for_api_route(None, None)
        .await
        .expect("legacy api-route acquire");
    assert_eq!(legacy_route.lease.id, tavily_id);

    // HTTP-project entry point: no project context means no provider
    // selection at all (None), for both the wrapper and the variant.
    assert!(
        proxy
            .acquire_key_for_http_project_provider(ProviderId::Brave, None, None)
            .await
            .expect("brave http-project acquire without context")
            .is_none()
    );
}

/// Arm the HTTP-global transient backoff exactly like the reconcile path does.
async fn arm_http_backoff(store: &KeyStore, key_id: &str, cooldown_until: i64, now: i64) {
    assert!(
        store
            .arm_api_key_transient_backoff(ApiKeyTransientBackoffArm {
                key_id,
                scope: HTTP_GLOBAL_BACKOFF_SCOPE,
                cooldown_until,
                retry_after_secs: 60,
                reason_code: None,
                source_request_log_id: None,
                now,
            })
            .await
            .expect("arm transient backoff")
            .is_some()
    );
}

/// Complete write-side-effect image for the strict cross-key-retry primitive:
/// the row count of every table a marking / quarantine / backoff / log /
/// affinity-rebind write could touch, plus the full `api_keys` row image
/// (status, recovery-window columns and the LRU `last_used_at` a lease bumps).
/// Comparing this before and after a `None` outcome proves the primitive
/// performed no marking write of any kind.
async fn retry_write_image(pool: &sqlx::SqlitePool) -> String {
    let mut parts = Vec::new();
    for table in [
        "api_key_transient_backoffs",
        "api_key_quarantines",
        "api_key_low_quota_depletions",
        "api_key_maintenance_records",
        "api_key_quota_sync_samples",
        "request_logs",
        "auth_token_logs",
        "mcp_sessions",
        "user_primary_api_key_affinity",
        "token_primary_api_key_affinity",
        "user_api_key_bindings",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = sqlx::query_scalar(&sql)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|err| panic!("count rows of {table}: {err}"));
        parts.push(format!("{table}={count}"));
    }
    let image: Vec<String> = sqlx::query_scalar(
        "SELECT id || ';' || status || ';' || COALESCE(status_changed_at, -1) || ';' || \
         COALESCE(last_used_at, -1) || ';' || COALESCE(quota_limit, -1) || ';' || \
         COALESCE(quota_remaining, -1) || ';' || COALESCE(quota_renews_at, -1) || ';' || \
         COALESCE(quota_synced_at, -1) || ';' || COALESCE(deleted_at, -1) FROM api_keys \
         ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("api_keys image");
    parts.push(format!("api_keys={image:?}"));
    parts.join("|")
}

/// Strict predicate (plan todo 5, QA happy): of three keys - A cooling down in
/// scope, B in the exclusion set - the retry acquire MUST return C, and it
/// never crosses into another provider's pool.
#[tokio::test]
async fn cross_key_retry_skips_cooled_and_excluded_keys_and_returns_the_survivor() {
    let db_path = temp_db_path("cross-key-retry-strict-survivor");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &[
            "tvly-ck-a".to_string(),
            "tvly-ck-b".to_string(),
            "tvly-ck-c".to_string(),
        ],
    )
    .await;
    // A healthy foreign-provider key: it must never satisfy a Tavily retry.
    seed_provider_keys(&store, ProviderId::Brave, &["brv-ck-a".to_string()]).await;

    let key_a = key_id(&store.pool, "tvly-ck-a").await;
    let key_b = key_id(&store.pool, "tvly-ck-b").await;
    let key_c = key_id(&store.pool, "tvly-ck-c").await;

    let now = clock.now_ts();
    arm_http_backoff(&store, &key_a, now + 60, now).await;

    let lease = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            None,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            std::slice::from_ref(&key_b),
        )
        .await
        .expect("strict retry acquire")
        .expect("key C is the only eligible candidate");
    std::println!(
        "[task-5][survivor] A(cooled)={key_a} B(excluded)={key_b} C={key_c} -> returned id={} secret={}",
        lease.id,
        lease.secret
    );
    assert_eq!(lease.id, key_c);
    assert_eq!(lease.secret, "tvly-ck-c");

    // The cooled key is never a fallback: excluding C too leaves nothing.
    assert!(
        store
            .acquire_key_for_cross_key_retry(
                ProviderId::Tavily,
                None,
                HTTP_GLOBAL_BACKOFF_SCOPE,
                &[key_b.clone(), key_c.clone()],
            )
            .await
            .expect("strict retry acquire")
            .is_none(),
        "a cooling key must never be reused as a retry fallback"
    );
    std::println!("[task-5][survivor] excluded=[B,C] with A cooled -> None (no cooled fallback)");
}

/// QA failure scenario: both `None` shapes (everything excluded, everything
/// cooled) leave ZERO write side effects - no new rows in any marking/log/
/// affinity table and a byte-identical `api_keys` image, so no lease touched
/// `last_used_at` either.
#[tokio::test]
async fn cross_key_retry_returns_none_without_write_side_effects_when_pool_unusable() {
    let db_path = temp_db_path("cross-key-retry-none-no-writes");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &["tvly-ck-x".to_string(), "tvly-ck-y".to_string()],
    )
    .await;
    seed_provider_keys(&store, ProviderId::Brave, &["brv-ck-y".to_string()]).await;

    let key_x = key_id(&store.pool, "tvly-ck-x").await;
    let key_y = key_id(&store.pool, "tvly-ck-y").await;
    let now = clock.now_ts();
    arm_http_backoff(&store, &key_x, now + 60, now).await;

    // Shape 1: the only un-cooled key is excluded.
    let before = retry_write_image(&store.pool).await;
    let all_excluded = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            None,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            &[key_x.clone(), key_y.clone()],
        )
        .await
        .expect("strict retry acquire");
    assert!(all_excluded.is_none());
    let after = retry_write_image(&store.pool).await;
    assert_eq!(before, after, "all-excluded None must not write anything");
    std::println!("[task-5][none] all excluded -> None; write image unchanged: {after}");

    // Shape 2: every key of the provider is cooling down (empty exclusion set),
    // and a healthy Brave key still must not be leased across providers.
    arm_http_backoff(&store, &key_y, now + 60, now).await;
    let before_cooled = retry_write_image(&store.pool).await;
    let all_cooled = store
        .acquire_key_for_cross_key_retry(ProviderId::Tavily, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire");
    assert!(all_cooled.is_none());
    let after_cooled = retry_write_image(&store.pool).await;
    assert_eq!(
        before_cooled, after_cooled,
        "all-cooled None must not write anything"
    );
    std::println!("[task-5][none] all cooled -> None; write image unchanged: {after_cooled}");
}

/// Exhausted keys are selectable ONLY after their provider recovery window
/// expired, decided by the shared policy sweep: Tavily at the UTC month flip,
/// SerpApi at `quota_renews_at` (RenewalDate revival), Exa never on a calendar
/// (success-only) - before that the strict acquire returns `None` instead of
/// burning a guaranteed-reject upstream call.
#[tokio::test]
async fn cross_key_retry_selects_exhausted_key_only_after_its_recovery_window() {
    let db_path = temp_db_path("cross-key-retry-exhausted-window");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-ck-m".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Serpapi, &["srp-ck-r".to_string()]).await;
    seed_provider_keys(&store, ProviderId::Exa, &["exa-ck-s".to_string()]).await;
    for secret in ["tvly-ck-m", "srp-ck-r", "exa-ck-s"] {
        assert!(
            store
                .mark_quota_exhausted(secret)
                .await
                .expect("seed exhausted")
        );
    }
    // SerpApi plan renewal from the /account sync (RenewalDate policy).
    sqlx::query("UPDATE api_keys SET quota_renews_at = ? WHERE api_key = ?")
        .bind(utc_ts("2026-04-10T00:00:00Z").timestamp())
        .bind("srp-ck-r")
        .execute(&store.pool)
        .await
        .expect("seed quota_renews_at");

    // Tavily: still inside the March window -> None; the UTC month flip
    // revives it and the same call then leases it.
    assert!(
        store
            .acquire_key_for_cross_key_retry(
                ProviderId::Tavily,
                None,
                HTTP_GLOBAL_BACKOFF_SCOPE,
                &[]
            )
            .await
            .expect("strict retry acquire")
            .is_none(),
        "exhausted Tavily key inside its month window is not selectable"
    );
    assert_eq!(key_status(&store.pool, "tvly-ck-m").await, "exhausted");
    clock.set_now_utc(utc_ts("2026-04-01T00:00:01Z"));
    let tavily_lease = store
        .acquire_key_for_cross_key_retry(ProviderId::Tavily, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire")
        .expect("month-flip revived key is selectable");
    assert_eq!(tavily_lease.secret, "tvly-ck-m");
    assert_eq!(key_status(&store.pool, "tvly-ck-m").await, "active");
    std::println!(
        "[task-5][window] tavily UtcMonth: before flip -> None, after flip -> id={} secret={}",
        tavily_lease.id,
        tavily_lease.secret
    );

    // SerpApi: strictly before quota_renews_at the key stays exhausted and
    // unselectable even though a month boundary passed; at the renewal
    // instant (`<=`) the sweep revives it and the strict acquire leases it.
    assert!(
        store
            .acquire_key_for_cross_key_retry(
                ProviderId::Serpapi,
                None,
                HTTP_GLOBAL_BACKOFF_SCOPE,
                &[]
            )
            .await
            .expect("strict retry acquire")
            .is_none(),
        "exhausted SerpApi key before quota_renews_at is not selectable"
    );
    clock.set_now_utc(utc_ts("2026-04-09T23:59:59Z"));
    assert!(
        store
            .acquire_key_for_cross_key_retry(
                ProviderId::Serpapi,
                None,
                HTTP_GLOBAL_BACKOFF_SCOPE,
                &[]
            )
            .await
            .expect("strict retry acquire")
            .is_none(),
        "still inside the SerpApi renewal window"
    );
    assert_eq!(key_status(&store.pool, "srp-ck-r").await, "exhausted");
    clock.set_now_utc(utc_ts("2026-04-10T00:00:00Z"));
    let serpapi_lease = store
        .acquire_key_for_cross_key_retry(ProviderId::Serpapi, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire")
        .expect("renewal-revived key is selectable");
    assert_eq!(serpapi_lease.secret, "srp-ck-r");
    assert_eq!(key_status(&store.pool, "srp-ck-r").await, "active");
    std::println!(
        "[task-5][window] serpapi RenewalDate: before -> None, at quota_renews_at -> id={} secret={}",
        serpapi_lease.id,
        serpapi_lease.secret
    );

    // Exa: SuccessOnly has no calendar window, so no boundary ever makes an
    // exhausted key selectable; only the success-restore path does.
    for boundary in ["2026-05-01T00:00:01Z", "2027-01-01T00:00:01Z"] {
        clock.set_now_utc(utc_ts(boundary));
        assert!(
            store
                .acquire_key_for_cross_key_retry(
                    ProviderId::Exa,
                    None,
                    HTTP_GLOBAL_BACKOFF_SCOPE,
                    &[]
                )
                .await
                .expect("strict retry acquire")
                .is_none(),
            "success-only Exa key must not become selectable on {boundary}"
        );
    }
    assert_eq!(key_status(&store.pool, "exa-ck-s").await, "exhausted");
    assert!(
        store
            .restore_active_status("exa-ck-s")
            .await
            .expect("exa success restore")
    );
    let exa_lease = store
        .acquire_key_for_cross_key_retry(ProviderId::Exa, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire")
        .expect("success-restored key is selectable");
    assert_eq!(exa_lease.secret, "exa-ck-s");
    std::println!(
        "[task-5][window] exa SuccessOnly: every calendar boundary -> None, after success-restore -> id={} secret={}",
        exa_lease.id,
        exa_lease.secret
    );
}

/// The low-quota depletion window is part of the strict predicate: a key
/// depleted THIS month is skipped, and it becomes selectable once the month
/// window rolled past the depletion row (row left untouched - no writes).
#[tokio::test]
async fn cross_key_retry_skips_low_quota_depleted_key_until_the_month_window_expires() {
    let db_path = temp_db_path("cross-key-retry-depletion-window");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-ck-d".to_string()]).await;
    let key_d = key_id(&store.pool, "tvly-ck-d").await;
    let month_start = utc_ts("2026-03-01T00:00:00Z").timestamp();
    sqlx::query(
        "INSERT INTO api_key_low_quota_depletions (key_id, month_start, threshold, \
         quota_remaining, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&key_d)
    .bind(month_start)
    .bind(15)
    .bind(15)
    .bind(clock.now_ts())
    .execute(&store.pool)
    .await
    .expect("seed depletion row");

    assert!(
        store
            .acquire_key_for_cross_key_retry(
                ProviderId::Tavily,
                None,
                HTTP_GLOBAL_BACKOFF_SCOPE,
                &[]
            )
            .await
            .expect("strict retry acquire")
            .is_none(),
        "a key depleted this month is not selectable"
    );
    std::println!("[task-5][depletion] inside March window -> None");

    clock.set_now_utc(utc_ts("2026-04-01T00:00:01Z"));
    let lease = store
        .acquire_key_for_cross_key_retry(ProviderId::Tavily, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire")
        .expect("expired depletion window makes the key selectable");
    assert_eq!(lease.id, key_d);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key_low_quota_depletions")
        .fetch_one(&store.pool)
        .await
        .expect("depletion row count");
    assert_eq!(
        rows, 1,
        "the primitive never deletes or rewrites marking rows"
    );
    std::println!(
        "[task-5][depletion] April (window expired) -> id={} secret={}; depletion rows still {rows}",
        lease.id,
        lease.secret
    );
}

/// The affinity candidate hierarchy is reused READ-ONLY: the token's primary
/// key wins over the LRU-first pool pick, the exclusion set still beats
/// affinity, a foreign-provider affinity row is skipped instead of leased, and
/// no affinity row is rebound or written.
#[tokio::test]
async fn cross_key_retry_prefers_token_affinity_candidate_without_rebinding() {
    let db_path = temp_db_path("cross-key-retry-affinity");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &["tvly-ck-p".to_string(), "tvly-ck-q".to_string()],
    )
    .await;
    seed_provider_keys(&store, ProviderId::Brave, &["brv-ck-p".to_string()]).await;

    let order: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM api_keys WHERE provider = ? AND status = 'active' AND deleted_at IS NULL \
         ORDER BY last_used_at ASC, id ASC",
    )
    .bind(ProviderId::Tavily.as_str())
    .fetch_all(&store.pool)
    .await
    .expect("tavily pool order");
    assert_eq!(order.len(), 2);
    let brave_id = key_id(&store.pool, "brv-ck-p").await;
    let token_id = "tok-cross-key-retry";

    // Affinity points at the key the pool ordering would NOT pick first.
    store
        .set_token_primary_api_key_affinity(token_id, None, &order[1])
        .await
        .expect("seed token affinity");
    let before = retry_write_image(&store.pool).await;

    let affinity_lease = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            Some(token_id),
            HTTP_GLOBAL_BACKOFF_SCOPE,
            &[],
        )
        .await
        .expect("strict retry acquire")
        .expect("affinity candidate is eligible");
    assert_eq!(
        affinity_lease.id, order[1],
        "the token affinity candidate wins over the LRU-first pool pick"
    );
    std::println!(
        "[task-5][affinity] pool order={order:?} token affinity={} -> returned id={} secret={}",
        order[1],
        affinity_lease.id,
        affinity_lease.secret
    );

    // The exclusion set outranks affinity: the same token falls through to the
    // pool tier.
    let pool_lease = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            Some(token_id),
            HTTP_GLOBAL_BACKOFF_SCOPE,
            std::slice::from_ref(&order[1]),
        )
        .await
        .expect("strict retry acquire")
        .expect("pool candidate remains");
    assert_eq!(pool_lease.id, order[0]);
    std::println!(
        "[task-5][affinity] excluded=[affinity key] -> pool tier id={} secret={}",
        pool_lease.id,
        pool_lease.secret
    );

    // A foreign-provider affinity row is skipped, never leased across
    // providers.
    store
        .set_token_primary_api_key_affinity(token_id, None, &brave_id)
        .await
        .expect("reseed foreign affinity");
    let foreign_lease = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            Some(token_id),
            HTTP_GLOBAL_BACKOFF_SCOPE,
            &[],
        )
        .await
        .expect("strict retry acquire")
        .expect("tavily pool candidate remains");
    assert!(foreign_lease.secret.starts_with("tvly-ck-"));
    assert_ne!(foreign_lease.id, brave_id);
    std::println!(
        "[task-5][affinity] foreign affinity={brave_id} -> skipped, returned id={} secret={}",
        foreign_lease.id,
        foreign_lease.secret
    );

    // No rebind: the only affinity writes above are the test's own seeds, and
    // no user-primary row was created by the primitive.
    let bound: String = sqlx::query_scalar(
        "SELECT api_key_id FROM token_primary_api_key_affinity WHERE token_id = ?",
    )
    .bind(token_id)
    .fetch_one(&store.pool)
    .await
    .expect("token affinity row");
    assert_eq!(bound, brave_id, "the test seed is the last affinity write");
    let user_primary_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_primary_api_key_affinity")
            .fetch_one(&store.pool)
            .await
            .expect("user affinity count");
    assert_eq!(user_primary_rows, 0);

    // The lease path only bumped `last_used_at`; no marking table gained a row
    // between the seed and the end of the run.
    let after = retry_write_image(&store.pool).await;
    for table in [
        "api_key_transient_backoffs",
        "api_key_quarantines",
        "api_key_low_quota_depletions",
        "api_key_maintenance_records",
        "api_key_quota_sync_samples",
        "request_logs",
        "auth_token_logs",
        "mcp_sessions",
        "user_primary_api_key_affinity",
        "token_primary_api_key_affinity",
        "user_api_key_bindings",
    ] {
        let before_count = before
            .split('|')
            .find_map(|part| part.strip_prefix(&format!("{table}=")))
            .expect("before count");
        let after_count = after
            .split('|')
            .find_map(|part| part.strip_prefix(&format!("{table}=")))
            .expect("after count");
        assert_eq!(before_count, after_count, "{table} gained a row");
    }
    std::println!("[task-5][affinity] marking tables unchanged across the whole run: {after}");
}
