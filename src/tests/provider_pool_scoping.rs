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
//! - MCP session key rebind write path (plan provider-key-429-retry todo 12):
//!   one atomic UPDATE moves both `upstream_session_id` and `upstream_key_id`
//!   while every other column (lease, rate-limit state, gateway mode, token /
//!   user ownership) stays byte-identical, the key-scoped revocation filters
//!   stop matching the old key and start matching the new one, and revoked or
//!   unknown proxy session ids are a no-op that inserts nothing.

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

/// Mirror the quota-sync write path: sync (or any other writer) setting
/// `quota_remaining` must be honored by every key selection site.
async fn set_quota_remaining(pool: &sqlx::SqlitePool, secret: &str, remaining: Option<i64>) {
    sqlx::query("UPDATE api_keys SET quota_remaining = ? WHERE api_key = ?")
        .bind(remaining)
        .bind(secret)
        .execute(pool)
        .await
        .expect("set quota_remaining");
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

/// A SerpApi key exhausted AFTER its stored renewal date must stay
/// exhausted on the RenewalDate sweep (churn guard, 2026-10-28): before the
/// `status_changed_at < quota_renews_at` clause, such a key was revived on
/// every acquire, immediately re-marked by the next 429 run-out body.
#[tokio::test]
async fn reset_monthly_skips_key_exhausted_after_renewal_date() {
    let db_path = temp_db_path("provider-pool-reset-monthly-post-renewal");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-04-12T00:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Serpapi,
        &["srp-post-renewal".to_string()],
    )
    .await;

    // Renewal date sits strictly BEFORE the exhaustion mark.
    let renews_at = utc_ts("2026-04-10T00:00:00Z").timestamp();
    sqlx::query("UPDATE api_keys SET quota_renews_at = ? WHERE api_key = ?")
        .bind(renews_at)
        .bind("srp-post-renewal")
        .execute(&store.pool)
        .await
        .expect("seed quota_renews_at");
    assert!(
        store
            .mark_quota_exhausted("srp-post-renewal")
            .await
            .expect("seed exhausted")
    );

    store
        .reset_monthly(ProviderId::Serpapi)
        .await
        .expect("serpapi reset");
    assert_eq!(
        key_status(&store.pool, "srp-post-renewal").await,
        "exhausted",
        "a key exhausted after its stored renewal date must not revive"
    );
    let _ = clock;
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

/// Column-ordered image of one `mcp_sessions` row, so a before/after diff can
/// name exactly the columns a write touched.
const MCP_SESSION_IMAGE_COLUMNS: &[&str] = &[
    "proxy_session_id",
    "upstream_session_id",
    "upstream_key_id",
    "auth_token_id",
    "user_id",
    "protocol_version",
    "last_event_id",
    "gateway_mode",
    "experiment_variant",
    "provider",
    "ab_bucket",
    "routing_subject_hash",
    "fallback_reason",
    "rate_limited_until",
    "last_rate_limited_at",
    "last_rate_limit_reason",
    "created_at",
    "updated_at",
    "expires_at",
    "revoked_at",
    "revoke_reason",
];

async fn mcp_session_row_image(
    pool: &sqlx::SqlitePool,
    proxy_session_id: &str,
) -> Vec<(String, String)> {
    let select = MCP_SESSION_IMAGE_COLUMNS
        .iter()
        .map(|column| format!("COALESCE(CAST({column} AS TEXT), '-')"))
        .collect::<Vec<_>>()
        .join(" || ';' || ");
    let flat: String = sqlx::query_scalar(&format!(
        "SELECT {select} FROM mcp_sessions WHERE proxy_session_id = ?"
    ))
    .bind(proxy_session_id)
    .fetch_one(pool)
    .await
    .expect("mcp session row image");
    MCP_SESSION_IMAGE_COLUMNS
        .iter()
        .map(|column| column.to_string())
        .zip(flat.split(';').map(str::to_string))
        .collect()
}

fn changed_mcp_session_columns(
    before: &[(String, String)],
    after: &[(String, String)],
) -> Vec<String> {
    before
        .iter()
        .zip(after.iter())
        .filter(|(before, after)| before.1 != after.1)
        .map(|(before, _)| before.0.clone())
        .collect()
}

/// A pinned upstream-mode session bound to `upstream_key_id`, written through
/// the production create path (no hand-rolled INSERT) so the rebind pins run
/// against the row shape the server actually produces.
async fn seed_pinned_mcp_session(
    store: &KeyStore,
    proxy_session_id: &str,
    upstream_session_id: &str,
    upstream_key_id: &str,
    token_id: &str,
    user_id: &str,
    now: i64,
) {
    store
        .create_or_replace_mcp_session(&McpSessionBinding {
            proxy_session_id: proxy_session_id.to_string(),
            upstream_session_id: Some(upstream_session_id.to_string()),
            upstream_key_id: Some(upstream_key_id.to_string()),
            auth_token_id: Some(token_id.to_string()),
            user_id: Some(user_id.to_string()),
            protocol_version: Some("2025-06-18".to_string()),
            last_event_id: None,
            gateway_mode: MCP_GATEWAY_MODE_UPSTREAM.to_string(),
            experiment_variant: MCP_EXPERIMENT_VARIANT_CONTROL.to_string(),
            ab_bucket: None,
            routing_subject_hash: None,
            fallback_reason: None,
            rate_limited_until: None,
            last_rate_limited_at: None,
            last_rate_limit_reason: None,
            created_at: now,
            updated_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            revoke_reason: None,
        })
        .await
        .expect("seed pinned mcp session");
}

async fn mcp_session_revocation(pool: &sqlx::SqlitePool, proxy_session_id: &str) -> (i64, String) {
    let flat: String = sqlx::query_scalar(
        "SELECT COALESCE(revoked_at, -1) || ';' || COALESCE(revoke_reason, '-') \
         FROM mcp_sessions WHERE proxy_session_id = ?",
    )
    .bind(proxy_session_id)
    .fetch_one(pool)
    .await
    .expect("mcp session revocation state");
    let mut parts = flat.splitn(2, ';');
    let revoked_at = parts
        .next()
        .expect("revoked_at part")
        .parse::<i64>()
        .expect("revoked_at parses");
    (
        revoked_at,
        parts.next().expect("revoke_reason part").to_string(),
    )
}

/// Acceptance (plan todo 12, happy QA "rebind -> read-back consistent"): ONE
/// atomic UPDATE moves BOTH identity columns; the proxy session id stays
/// stable, no second row appears, and every other column - lease, rate-limit
/// state, gateway mode, token / user ownership - is byte-identical.
#[tokio::test]
async fn mcp_session_rebind_writes_both_upstream_identity_columns_in_one_update() {
    let db_path = temp_db_path("mcp-session-rebind-identity");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &["tvly-rebind-old".to_string(), "tvly-rebind-new".to_string()],
    )
    .await;
    let old_key = key_id(&store.pool, "tvly-rebind-old").await;
    let new_key = key_id(&store.pool, "tvly-rebind-new").await;

    let now = clock.now_ts();
    let proxy_session_id = "psess-rebind-identity";
    seed_pinned_mcp_session(
        &store,
        proxy_session_id,
        "up-sess-old",
        &old_key,
        "tok-rebind-identity",
        "user-rebind-identity",
        now,
    )
    .await;

    let before = mcp_session_row_image(&store.pool, proxy_session_id).await;
    let bound = store
        .get_active_mcp_session(proxy_session_id, now)
        .await
        .expect("read session")
        .expect("seeded session is active");
    assert_eq!(bound.upstream_session_id.as_deref(), Some("up-sess-old"));
    assert_eq!(bound.upstream_key_id.as_deref(), Some(old_key.as_str()));

    clock.set_now_ts(now + 30);
    store
        .rebind_mcp_session_upstream_key(proxy_session_id, "up-sess-new", &new_key)
        .await
        .expect("rebind mcp session");

    let rebound = store
        .get_active_mcp_session(proxy_session_id, now + 30)
        .await
        .expect("read session")
        .expect("a rebind keeps the session active");
    assert_eq!(
        rebound.upstream_session_id.as_deref(),
        Some("up-sess-new"),
        "upstream_session_id must move to the re-initialized upstream session"
    );
    assert_eq!(
        rebound.upstream_key_id.as_deref(),
        Some(new_key.as_str()),
        "upstream_key_id must move to the new key"
    );
    assert_eq!(
        rebound.proxy_session_id, proxy_session_id,
        "the proxy session id is stable across the migration"
    );
    assert_eq!(rebound.revoked_at, None);

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mcp_sessions")
        .fetch_one(&store.pool)
        .await
        .expect("count mcp sessions");
    assert_eq!(rows, 1, "a rebind updates the row, it never inserts one");

    let after = mcp_session_row_image(&store.pool, proxy_session_id).await;
    let changed = changed_mcp_session_columns(&before, &after);
    assert_eq!(
        changed,
        ["upstream_session_id", "upstream_key_id", "updated_at"],
        "the single UPDATE writes both identity columns and nothing else"
    );
    assert_eq!(rebound.created_at, now);
    assert_eq!(
        rebound.updated_at,
        now + 30,
        "updated_at follows the injected backend clock"
    );
    assert_eq!(
        rebound.expires_at,
        now + 3600,
        "the rebind never touches the lease column"
    );
    assert_eq!(rebound.rate_limited_until, None);
    assert_eq!(rebound.last_rate_limited_at, None);
    assert_eq!(rebound.last_rate_limit_reason, None);
    assert_eq!(rebound.protocol_version.as_deref(), Some("2025-06-18"));
    assert_eq!(
        rebound.auth_token_id.as_deref(),
        Some("tok-rebind-identity")
    );
    assert_eq!(rebound.user_id.as_deref(), Some("user-rebind-identity"));
    assert_eq!(rebound.gateway_mode, MCP_GATEWAY_MODE_UPSTREAM);
    assert_eq!(rebound.experiment_variant, MCP_EXPERIMENT_VARIANT_CONTROL);
    std::println!(
        "[task-12][identity] {proxy_session_id} old_key={old_key} new_key={new_key} \
         up-sess-old -> {} ; changed columns={changed:?}",
        rebound.upstream_session_id.as_deref().unwrap_or("-")
    );
}

/// Acceptance (plan todo 12): the key-scoped revocation filters follow the
/// row. Failure QA "old key revoke -> session survives": after the rebind
/// neither `revoke_mcp_sessions_for_token_key` nor
/// `revoke_mcp_sessions_for_user_key` matches on the OLD key, while both match
/// on the NEW key (one session pinned per filter).
#[tokio::test]
async fn mcp_session_rebind_moves_key_revocation_filters_to_the_new_key() {
    let db_path = temp_db_path("mcp-session-rebind-revocation");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &[
            "tvly-rebind-revoked-old".to_string(),
            "tvly-rebind-revoked-new".to_string(),
        ],
    )
    .await;
    let old_key = key_id(&store.pool, "tvly-rebind-revoked-old").await;
    let new_key = key_id(&store.pool, "tvly-rebind-revoked-new").await;

    let now = clock.now_ts();
    let token_session = "psess-rebind-token-filter";
    let user_session = "psess-rebind-user-filter";
    seed_pinned_mcp_session(
        &store,
        token_session,
        "up-sess-token-old",
        &old_key,
        "tok-rebind-token",
        "user-rebind-token",
        now,
    )
    .await;
    seed_pinned_mcp_session(
        &store,
        user_session,
        "up-sess-user-old",
        &old_key,
        "tok-rebind-user",
        "user-rebind-user",
        now,
    )
    .await;
    store
        .rebind_mcp_session_upstream_key(token_session, "up-sess-token-new", &new_key)
        .await
        .expect("rebind token-filter session");
    store
        .rebind_mcp_session_upstream_key(user_session, "up-sess-user-new", &new_key)
        .await
        .expect("rebind user-filter session");

    // Old-key revocation no longer matches either session.
    store
        .revoke_mcp_sessions_for_token_key("tok-rebind-token", &old_key, "primary_api_key_rebound")
        .await
        .expect("revoke by old token key");
    store
        .revoke_mcp_sessions_for_user_key("user-rebind-user", &old_key, "primary_api_key_rebound")
        .await
        .expect("revoke by old user key");
    for proxy_session_id in [token_session, user_session] {
        let survived = store
            .get_active_mcp_session(proxy_session_id, now)
            .await
            .expect("read session")
            .unwrap_or_else(|| panic!("{proxy_session_id} must survive old-key revocation"));
        assert_eq!(survived.revoked_at, None);
        assert_eq!(survived.upstream_key_id.as_deref(), Some(new_key.as_str()));
    }
    let revoked_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mcp_sessions WHERE revoked_at IS NOT NULL")
            .fetch_one(&store.pool)
            .await
            .expect("count revoked sessions");
    assert_eq!(
        revoked_rows, 0,
        "an old-key revocation filter must not match a rebound session"
    );
    std::println!(
        "[task-12][revocation] old key {old_key} revoked via both filters -> 0 rows matched"
    );

    // New-key revocation matches: token filter first.
    store
        .revoke_mcp_sessions_for_token_key("tok-rebind-token", &new_key, "pinned_key_unavailable")
        .await
        .expect("revoke by new token key");
    let (token_revoked_at, token_reason) = mcp_session_revocation(&store.pool, token_session).await;
    assert!(
        token_revoked_at > 0,
        "the token filter revokes the rebound row"
    );
    assert_eq!(token_reason, "pinned_key_unavailable");
    assert!(
        store
            .get_active_mcp_session(token_session, now)
            .await
            .expect("read session")
            .is_none(),
        "a revoked session is no longer active"
    );
    assert_eq!(
        mcp_session_revocation(&store.pool, user_session).await.0,
        -1,
        "the token-scoped filter must not reach the other session"
    );

    // ... then the user filter.
    store
        .revoke_mcp_sessions_for_user_key("user-rebind-user", &new_key, "pinned_key_unavailable")
        .await
        .expect("revoke by new user key");
    let (user_revoked_at, user_reason) = mcp_session_revocation(&store.pool, user_session).await;
    assert!(
        user_revoked_at > 0,
        "the user filter revokes the rebound row"
    );
    assert_eq!(user_reason, "pinned_key_unavailable");
    assert!(
        store
            .get_active_mcp_session(user_session, now)
            .await
            .expect("read session")
            .is_none(),
        "a revoked session is no longer active"
    );
    std::println!(
        "[task-12][revocation] new key {new_key} matched both filters: \
         token_session revoked_at={token_revoked_at} user_session revoked_at={user_revoked_at}"
    );
}

/// Guard: the rebind only ever moves a LIVE row. A revoked session keeps its
/// old identity and its revocation record byte-identically, and an unknown
/// proxy session id is a no-op that inserts nothing.
#[tokio::test]
async fn mcp_session_rebind_ignores_revoked_and_unknown_sessions() {
    let db_path = temp_db_path("mcp-session-rebind-guard");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &[
            "tvly-rebind-guard-old".to_string(),
            "tvly-rebind-guard-new".to_string(),
        ],
    )
    .await;
    let old_key = key_id(&store.pool, "tvly-rebind-guard-old").await;
    let new_key = key_id(&store.pool, "tvly-rebind-guard-new").await;

    let now = clock.now_ts();
    let proxy_session_id = "psess-rebind-guard";
    seed_pinned_mcp_session(
        &store,
        proxy_session_id,
        "up-sess-guard-old",
        &old_key,
        "tok-rebind-guard",
        "user-rebind-guard",
        now,
    )
    .await;
    store
        .revoke_mcp_session(proxy_session_id, "pinned_key_unavailable")
        .await
        .expect("revoke session");
    let before = mcp_session_row_image(&store.pool, proxy_session_id).await;

    clock.set_now_ts(now + 60);
    store
        .rebind_mcp_session_upstream_key(proxy_session_id, "up-sess-guard-new", &new_key)
        .await
        .expect("rebind of a revoked session is not an error");
    let after = mcp_session_row_image(&store.pool, proxy_session_id).await;
    assert_eq!(
        before, after,
        "a revoked session is never rebound: the row image must be byte-identical"
    );

    store
        .rebind_mcp_session_upstream_key("psess-never-created", "up-sess-ghost", &new_key)
        .await
        .expect("rebind of an unknown session is not an error");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mcp_sessions")
        .fetch_one(&store.pool)
        .await
        .expect("count mcp sessions");
    assert_eq!(rows, 1, "an unknown proxy session id inserts nothing");
    std::println!(
        "[task-12][guard] revoked row untouched (upstream_key_id={old_key}) and \
         psess-never-created inserted nothing; rows={rows}"
    );
}

/// Zombie-key regression (plan tavily-zombie-key-exclusion todo 1): an ACTIVE
/// key whose synced `quota_remaining` is 0 must be excluded from EVERY
/// selection path - the 3-tier acquire, the backoff-aware acquire (and its
/// `acquire_active_key_excluding` fallback), the strict cross-key retry, the
/// retry-eligibility precheck, and the MCP rebind candidate listing. The
/// healthy sibling with a positive remaining keeps being leased.
#[tokio::test]
async fn zero_remaining_active_keys_are_never_selected_by_any_acquire_path() {
    let db_path = temp_db_path("zombie-key-zero-remaining-unselectable");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(
        &store,
        ProviderId::Tavily,
        &["tvly-zk-dead".to_string(), "tvly-zk-live".to_string()],
    )
    .await;
    set_quota_remaining(&store.pool, "tvly-zk-dead", Some(0)).await;
    set_quota_remaining(&store.pool, "tvly-zk-live", Some(500)).await;

    let dead_id = key_id(&store.pool, "tvly-zk-dead").await;
    let live_id = key_id(&store.pool, "tvly-zk-live").await;

    // acquire_key_for_provider tier-1 never leases the zero-remaining key.
    let lease = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("acquire_key_for_provider leases the healthy key");
    assert_eq!(lease.id, live_id);

    // The backoff-aware acquire (QueryBuilder site) also skips it.
    let lease = store
        .acquire_key_avoiding_transient_backoff_excluding(
            ProviderId::Tavily,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            None,
        )
        .await
        .expect("backoff-aware acquire leases the healthy key");
    assert_eq!(lease.id, live_id);

    // With the healthy key excluded the zombie is NOT a hidden fallback
    // (exercises acquire_active_key_excluding via the excluded branch).
    let err = store
        .acquire_key_avoiding_transient_backoff_excluding(
            ProviderId::Tavily,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            Some(&live_id),
        )
        .await
        .expect_err("only the zombie remains: no lease");
    assert!(matches!(err, ProxyError::NoAvailableKeys));

    // select_cross_key_retry_key via its public caller: excluding the zombie
    // leases the healthy key; excluding the healthy key returns None instead
    // of resurrecting the zombie.
    let retry = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            None,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            std::slice::from_ref(&dead_id),
        )
        .await
        .expect("strict retry acquire");
    assert_eq!(
        retry.map(|lease| lease.id),
        Some(live_id.clone()),
        "retry skips the zero-remaining key and leases the healthy one"
    );
    let retry = store
        .acquire_key_for_cross_key_retry(
            ProviderId::Tavily,
            None,
            HTTP_GLOBAL_BACKOFF_SCOPE,
            std::slice::from_ref(&live_id),
        )
        .await
        .expect("strict retry acquire");
    assert!(
        retry.is_none(),
        "zero-remaining key must never be a retry candidate"
    );

    // acquire_active_key_excluding agrees on both branches.
    let lease = store
        .acquire_active_key_excluding(ProviderId::Tavily, None)
        .await
        .expect("active acquire leases the healthy key");
    assert_eq!(lease.id, live_id);
    let err = store
        .acquire_active_key_excluding(ProviderId::Tavily, Some(&live_id))
        .await
        .expect_err("excluding the healthy key leaves only the zombie");
    assert!(matches!(err, ProxyError::NoAvailableKeys));

    // has_available_active_key_excluding (retry eligibility precheck) agrees
    // with the acquire paths.
    assert!(
        store
            .has_available_active_key_excluding(ProviderId::Tavily, None)
            .await
            .expect("availability check")
    );
    assert!(
        !store
            .has_available_active_key_excluding(ProviderId::Tavily, Some(&live_id))
            .await
            .expect("availability check"),
        "with the healthy key excluded only the zombie remains"
    );

    // MCP rebind candidates skip the zombie too.
    let candidates = store
        .list_mcp_session_candidate_key_ids(ProviderId::Tavily)
        .await
        .expect("mcp candidate listing");
    assert_eq!(candidates, vec![live_id.clone()]);

    std::eprintln!(
        "[zombie][mixed-pool] dead={dead_id} excluded everywhere; live={live_id} leased on all paths"
    );
}

/// Zombie-only pool: the zombie is still ACTIVE (not exhausted), so the
/// tier-2/3 exhausted fallbacks stay empty and every path reports the pool
/// as unusable instead of burning a guaranteed-432 request.
#[tokio::test]
async fn zero_remaining_only_pool_reports_unavailable_on_every_path() {
    let db_path = temp_db_path("zombie-key-only-pool-unavailable");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-zk-only".to_string()]).await;
    set_quota_remaining(&store.pool, "tvly-zk-only", Some(0)).await;

    assert!(
        !store
            .has_available_active_key_excluding(ProviderId::Tavily, None)
            .await
            .expect("availability check"),
        "a pool of zero-remaining keys only is not available"
    );
    let err = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect_err("zombie-only pool has no lease");
    assert!(matches!(err, ProxyError::NoAvailableKeys));
    let err = store
        .acquire_active_key_excluding(ProviderId::Tavily, None)
        .await
        .expect_err("zombie-only pool has no active lease");
    assert!(matches!(err, ProxyError::NoAvailableKeys));
    assert!(
        store
            .list_mcp_session_candidate_key_ids(ProviderId::Tavily)
            .await
            .expect("mcp candidate listing")
            .is_empty()
    );
    let retry = store
        .acquire_key_for_cross_key_retry(ProviderId::Tavily, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire");
    assert!(retry.is_none(), "zombie-only pool yields no retry key");

    std::eprintln!("[zombie][only-pool] every selection path reports empty");
}

/// NULL `quota_remaining` means never synced - or an unlimited provider month
/// (Brave convention) - and MUST stay selectable.
#[tokio::test]
async fn null_quota_remaining_keys_stay_selectable() {
    let db_path = temp_db_path("zombie-key-null-remaining-selectable");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Brave, &["brv-zk-null".to_string()]).await;
    let remaining: Option<i64> =
        sqlx::query_scalar("SELECT quota_remaining FROM api_keys WHERE api_key = ?")
            .bind("brv-zk-null")
            .fetch_one(&store.pool)
            .await
            .expect("quota_remaining lookup");
    assert_eq!(remaining, None, "freshly synced keys start unsynced (NULL)");

    let lease = store
        .acquire_key_for_provider(ProviderId::Brave)
        .await
        .expect("NULL-remaining key stays selectable");
    assert_eq!(lease.secret, "brv-zk-null");
    assert!(
        store
            .has_available_active_key_excluding(ProviderId::Brave, None)
            .await
            .expect("availability check")
    );

    std::eprintln!(
        "[zombie][null-passes] NULL remaining leased id={}",
        lease.id
    );
}

/// No sticky state: once a later sync raises `quota_remaining` above 0 the
/// key becomes selectable again without any status-machine involvement.
#[tokio::test]
async fn zero_remaining_key_becomes_selectable_again_after_positive_sync() {
    let db_path = temp_db_path("zombie-key-positive-sync-recovers");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, _clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-zk-heal".to_string()]).await;
    set_quota_remaining(&store.pool, "tvly-zk-heal", Some(0)).await;

    let err = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect_err("zero remaining: no lease");
    assert!(matches!(err, ProxyError::NoAvailableKeys));

    set_quota_remaining(&store.pool, "tvly-zk-heal", Some(250)).await;
    let lease = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("positive remaining restores selectability");
    assert_eq!(lease.secret, "tvly-zk-heal");

    std::eprintln!(
        "[zombie][heal] 0 -> 250 remaining re-enabled leasing id={}",
        lease.id
    );
}

/// Month-flip regression: `reset_monthly` at the UTC month start flips an
/// exhausted key back to active and the depletion NOT EXISTS window resets
/// with the new month_start - exactly the resurrection path that created the
/// zombie fleet. A resurrected key whose `quota_remaining` is still 0 must
/// STILL be unselectable on every path.
#[tokio::test]
async fn month_flip_resurrected_zero_remaining_key_stays_unselectable() {
    let db_path = temp_db_path("zombie-key-month-flip-resurrection");
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-03-15T12:00:00Z"));
    let store = KeyStore::new_with_time(&db_str, backend_time)
        .await
        .expect("store");

    seed_provider_keys(&store, ProviderId::Tavily, &["tvly-zk-flip".to_string()]).await;
    set_quota_remaining(&store.pool, "tvly-zk-flip", Some(0)).await;
    assert!(
        store
            .mark_quota_exhausted("tvly-zk-flip")
            .await
            .expect("seed exhausted")
    );
    assert_eq!(key_status(&store.pool, "tvly-zk-flip").await, "exhausted");

    // Flip the UTC month: reset_monthly resurrects the key to active while
    // the (empty) depletion window restarts for the new month.
    clock.set_now_utc(utc_ts("2026-04-01T00:00:01Z"));
    store
        .reset_monthly(ProviderId::Tavily)
        .await
        .expect("month-flip reset");
    assert_eq!(
        key_status(&store.pool, "tvly-zk-flip").await,
        "active",
        "reset_monthly must have resurrected the key for this regression to matter"
    );
    let remaining: Option<i64> =
        sqlx::query_scalar("SELECT quota_remaining FROM api_keys WHERE api_key = ?")
            .bind("tvly-zk-flip")
            .fetch_one(&store.pool)
            .await
            .expect("quota_remaining lookup");
    assert_eq!(
        remaining,
        Some(0),
        "the resurrection must not fake a balance"
    );

    // The resurrected zombie stays unselectable on every path.
    let err = store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect_err("resurrected zero-remaining key must not lease");
    assert!(matches!(err, ProxyError::NoAvailableKeys));
    assert!(
        !store
            .has_available_active_key_excluding(ProviderId::Tavily, None)
            .await
            .expect("availability check"),
        "resurrected zero-remaining key must not count as available"
    );
    let retry = store
        .acquire_key_for_cross_key_retry(ProviderId::Tavily, None, HTTP_GLOBAL_BACKOFF_SCOPE, &[])
        .await
        .expect("strict retry acquire");
    assert!(
        retry.is_none(),
        "resurrected zero-remaining key must not be a retry candidate"
    );
    assert!(
        store
            .list_mcp_session_candidate_key_ids(ProviderId::Tavily)
            .await
            .expect("mcp candidate listing")
            .is_empty()
    );

    std::eprintln!("[zombie][month-flip] resurrected active zombie stays unselectable");
}
