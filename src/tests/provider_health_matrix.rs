//! Plan todo 18 — provider × signal health matrix (DB-level pins) + alert
//! provider-field flow + M8 negative assertion + FU-A full-path alias pins.
//!
//! Coverage map (acceptance, plan L231):
//! - Exhausted  → `mark_quota_exhausted` + `api_key_maintenance_records`
//!   audit row + `api_key_exhausted` alert carrying `provider` (existing type
//!   key; NO new alert kinds, M18).
//! - AuthFailed → the existing quarantine machinery (`api_key_quarantines`
//!   row mirroring Tavily auth-fail behavior) + `auto_quarantine` maintenance
//!   row + `upstream_key_blocked` alert carrying `provider`.
//! - RateLimited → per-provider backoff SIZING on the shared `http_global`
//!   scope (the scope/reason columns the committed goldens pin): Brave
//!   second-scale, SerpApi hour-scale, Exa Retry-After via the existing
//!   minute-ladder clamp; arm→success-clear cycle per provider.
//! - Success on any provider path → `restore_active_status` +
//!   `auto_restore_active` maintenance row (Exa SuccessOnly recovery
//!   completes here; SerpApi renewal-date + Brave month-window calendar
//!   recovery stays pinned by `provider_pool_scoping::reset_monthly_recovers_
//!   each_provider_by_policy` — referenced, not reimplemented).
//! - M8 (binding): ZERO `api_key_low_quota_depletions` rows for
//!   serpapi/exa/brave after exhaustion+harvest+restore flows even with
//!   `quota_remaining <= threshold` seeded; Tavily-432 positive control
//!   proves the assertion can fire. 432 gate untouched.
//! - Alert wire shape: new-provider events carry `provider`; Tavily/legacy
//!   events serialize WITHOUT the `provider` key (byte-stable wire JSON) and
//!   the projection payload_json round-trips both shapes (raw CTE +
//!   projected read paths).
//! - FU-A: the three ADDITIVE brave full-path aliases classify the registered
//!   native paths onto the canonical kinds/labels/buckets.

use super::*;

use crate::backend_time::{BackendTime, ManualBackendTime};
use crate::providers::ProviderId;

fn utc_ts(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("valid rfc3339 fixture timestamp")
        .with_timezone(&chrono::Utc)
}

async fn seed_provider_key(proxy: &TavilyProxy, provider: ProviderId, secret: &str) -> String {
    proxy
        .key_store
        .sync_keys(provider, &[secret.to_string()])
        .await
        .expect("seed provider key");
    sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ?")
        .bind(secret)
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("key id lookup")
}

async fn key_status(proxy: &TavilyProxy, secret: &str) -> String {
    sqlx::query_scalar("SELECT status FROM api_keys WHERE api_key = ?")
        .bind(secret)
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("key status lookup")
}

async fn maintenance_rows(proxy: &TavilyProxy, key_id: &str, operation_code: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM api_key_maintenance_records WHERE key_id = ? AND operation_code = ?",
    )
    .bind(key_id)
    .bind(operation_code)
    .fetch_one(&proxy.key_store.pool)
    .await
    .expect("maintenance count")
}

async fn active_quarantine_reason(proxy: &TavilyProxy, key_id: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT reason_code FROM api_key_quarantines WHERE key_id = ? AND cleared_at IS NULL",
    )
    .bind(key_id)
    .fetch_optional(&proxy.key_store.pool)
    .await
    .expect("quarantine lookup")
}

async fn depletion_rows(proxy: &TavilyProxy) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM api_key_low_quota_depletions")
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("depletion count")
}

async fn active_backoff(proxy: &TavilyProxy, key_id: &str) -> Option<(String, String, i64)> {
    sqlx::query(
        "SELECT scope, reason_code, retry_after_secs FROM api_key_transient_backoffs \
         WHERE key_id = ? AND cooldown_until > ? \
         ORDER BY cooldown_until DESC LIMIT 1",
    )
    .bind(key_id)
    .bind(proxy.backend_time.now_ts())
    .fetch_optional(&proxy.key_store.pool)
    .await
    .expect("backoff lookup")
    .map(|row| {
        (
            row.get::<Option<String>, _>("scope").unwrap_or_default(),
            row.get::<Option<String>, _>("reason_code")
                .unwrap_or_default(),
            row.get::<Option<i64>, _>("retry_after_secs").unwrap_or(-1),
        )
    })
}

fn exhausted_analysis(upstream_status: i64) -> AttemptAnalysis {
    AttemptAnalysis {
        status: OUTCOME_QUOTA_EXHAUSTED,
        tavily_status_code: Some(upstream_status),
        key_health_action: KeyHealthAction::MarkExhausted,
        failure_kind: None,
        key_effect: KeyEffect::none(),
        api_key_id: None,
    }
}

fn auth_failed_analysis(upstream_status: i64) -> AttemptAnalysis {
    AttemptAnalysis {
        status: OUTCOME_ERROR,
        tavily_status_code: Some(upstream_status),
        key_health_action: KeyHealthAction::Quarantine(QuarantineDecision {
            reason_code: BLOCKED_KEY_REASON_INVALID_API_KEY.to_string(),
            reason_summary: "matrix auth-fail".to_string(),
            reason_detail: "matrix".to_string(),
        }),
        failure_kind: Some(FAILURE_KIND_UPSTREAM_ACCOUNT_DEACTIVATED_401.to_string()),
        key_effect: KeyEffect::none(),
        api_key_id: None,
    }
}

fn success_analysis() -> AttemptAnalysis {
    AttemptAnalysis {
        status: OUTCOME_SUCCESS,
        tavily_status_code: Some(200),
        key_health_action: KeyHealthAction::None,
        failure_kind: None,
        key_effect: KeyEffect::none(),
        api_key_id: None,
    }
}

async fn spawn_matrix_proxy(prefix: &str) -> (TavilyProxy, ManualBackendTime) {
    let db_path = temp_db_path(prefix);
    let db_str = db_path.to_string_lossy().to_string();
    let (backend_time, clock) = BackendTime::manual(utc_ts("2026-09-15T12:00:00Z"));
    let proxy = TavilyProxy::with_options_and_time(
        Vec::<String>::new(),
        DEFAULT_UPSTREAM,
        &db_str,
        TavilyProxyOptions::from_database_path(&db_str),
        backend_time,
    )
    .await
    .expect("matrix proxy created");
    (proxy, clock)
}

// ---------------------------------------------------------------------------
// RateLimited: per-provider sizing matrix (shared http_global scope).
// ---------------------------------------------------------------------------

/// SerpApi hour-scale sizing (D6 "hour scope"): no Retry-After → the full
/// hour default; parsed Retry-After honored from the minute-ladder floor up
/// to the hour ceiling (the committed T3 pin 45→45 stays byte-stable).
#[test]
fn serpapi_rate_limited_backoff_is_hour_scale() {
    // Given the todo-3 signal with no Retry-After (mock/live SerpApi hourly
    // 429s carry none), When sized for SerpApi, Then the hour default arms.
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Serpapi,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: None
            },
            utc_ts("2026-09-15T12:00:00Z").timestamp(),
        ),
        Some(3600),
        "hour-scale default"
    );
    // A parsed Retry-After inside the envelope is honored (T3 pin: 45→45)...
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Serpapi,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(120)
            },
            0,
        ),
        Some(120)
    );
    // ...and clamped on both ends: the minute-ladder floor below, the hour
    // ceiling above.
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Serpapi,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(2)
            },
            0,
        ),
        Some(30)
    );
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Serpapi,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(7200)
            },
            0,
        ),
        Some(3600)
    );
}

/// Brave second-scale + Exa minute-ladder pins: committed T13/T14 behavior
/// stays byte-identical (goldens pin brave=2s and exa Retry-After 2→30).
#[test]
fn brave_second_scale_and_exa_minute_ladder_pins_unchanged() {
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Brave,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: None
            },
            0,
        ),
        Some(2),
        "brave second-scale default"
    );
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Brave,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(120)
            },
            0,
        ),
        Some(30),
        "brave clamps to the 30s second-scale ceiling"
    );
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Exa,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(2)
            },
            0,
        ),
        Some(30),
        "exa Retry-After through the existing minute-ladder floor (golden)"
    );
    assert_eq!(
        TavilyProxy::effective_provider_rate_limited_backoff_secs(
            ProviderId::Exa,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(120)
            },
            0,
        ),
        Some(120),
        "exa Retry-After honored inside the ladder"
    );
}

/// Arm→success semantics per provider on the shared scope: the 429 row shape
/// the goldens pin (scope http_global, reason upstream_rate_limited_429,
/// per-provider sizing) expires by cooldown and is NOT cleared by the
/// success path (existing Tavily semantics: `clear_transient_backoffs_after_
/// success` clears only 403-blocked rows, which the second phase pins).
#[tokio::test]
async fn rate_limited_arm_and_success_clear_cycle_per_provider() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-backoff-cycle").await;
    for provider in [ProviderId::Serpapi, ProviderId::Exa, ProviderId::Brave] {
        let secret = format!("mtx-rl-{}", provider.as_str());
        let key_id = seed_provider_key(&proxy, provider, &secret).await;
        let now = proxy.backend_time.now_ts();
        let sized = TavilyProxy::effective_provider_rate_limited_backoff_secs(
            provider,
            ProviderFailureSignal::RateLimited {
                retry_after_secs: None,
            },
            now,
        )
        .expect("rate-limited sizing");
        proxy
            .key_store
            .arm_api_key_transient_backoff(ApiKeyTransientBackoffArm {
                key_id: &key_id,
                scope: HTTP_GLOBAL_BACKOFF_SCOPE,
                cooldown_until: now + sized,
                retry_after_secs: sized,
                reason_code: Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
                source_request_log_id: None,
                now,
            })
            .await
            .expect("arm backoff");
        let (scope, reason, retry_after) = active_backoff(&proxy, &key_id)
            .await
            .expect("armed row present");
        assert_eq!(scope, HTTP_GLOBAL_BACKOFF_SCOPE);
        assert_eq!(reason, FAILURE_KIND_UPSTREAM_RATE_LIMITED_429);
        assert_eq!(retry_after, sized);
        // The key STAYS active under a transient 429 (never exhausted).
        assert_eq!(key_status(&proxy, &secret).await, "active");

        // Success on the same key leaves the 429 row to its cooldown expiry
        // (house semantics, provider-agnostic) — the clear lane targets the
        // 403-blocked rows pinned in the second phase below.
        let lease = proxy
            .key_store
            .acquire_key_for_provider(provider)
            .await
            .expect("provider lease");
        assert_eq!(lease.id, key_id);
        let effect = proxy
            .clear_transient_backoffs_after_success(&lease.id, "/matrix", None)
            .await
            .expect("success clear pass over a 429 row");
        assert_eq!(effect.code.as_str(), KEY_EFFECT_NONE);
        assert!(
            active_backoff(&proxy, &key_id).await.is_some(),
            "429 backoffs expire by cooldown, not by success clearing"
        );

        // Phase 2 — the 403-blocked flavor IS cleared by the success path
        // (same lane the six provider record paths call), with its audit row.
        proxy
            .key_store
            .arm_api_key_transient_backoff(ApiKeyTransientBackoffArm {
                key_id: &key_id,
                scope: HTTP_GLOBAL_BACKOFF_SCOPE,
                cooldown_until: now + sized,
                retry_after_secs: sized,
                reason_code: Some(FAILURE_KIND_UPSTREAM_UNKNOWN_403),
                source_request_log_id: None,
                now,
            })
            .await
            .expect("arm 403 backoff");
        let effect = proxy
            .clear_transient_backoffs_after_success(&lease.id, "/matrix", None)
            .await
            .expect("success clear");
        assert_eq!(effect.code.as_str(), KEY_EFFECT_TRANSIENT_BACKOFF_CLEARED);
        assert!(active_backoff(&proxy, &key_id).await.is_none());
        assert_eq!(
            maintenance_rows(&proxy, &key_id, MAINTENANCE_OP_AUTO_CLEAR_TRANSIENT_BACKOFF).await,
            1
        );
    }
}

// ---------------------------------------------------------------------------
// Exhausted / AuthFailed / Success per provider (reconcile DB contract).
// ---------------------------------------------------------------------------

/// Exhausted per provider: status flip + audit maintenance row + effect;
/// M8 — zero depletion rows even with quota_remaining seeded at 0.
#[tokio::test]
async fn exhausted_signal_marks_exhausted_with_maintenance_row_per_provider() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-exhausted").await;
    // Provider exhaustion statuses from the committed classifier vocabulary:
    // serpapi run-out = 429, exa budget tags = 402, brave QUOTA_LIMITED = 402.
    for (provider, upstream_status) in [
        (ProviderId::Serpapi, 429i64),
        (ProviderId::Exa, 402),
        (ProviderId::Brave, 402),
    ] {
        let secret = format!("mtx-ex-{}", provider.as_str());
        let key_id = seed_provider_key(&proxy, provider, &secret).await;
        // M8 adversarial seed: remaining AT ZERO (under any threshold) so a
        // wrongly-generalized depletion gate WOULD write a row.
        sqlx::query("UPDATE api_keys SET quota_remaining = 0, quota_limit = 100 WHERE id = ?")
            .bind(&key_id)
            .execute(&proxy.key_store.pool)
            .await
            .expect("seed zero remaining");
        let lease = proxy
            .key_store
            .acquire_key_for_provider(provider)
            .await
            .expect("lease");
        let effect = proxy
            .reconcile_key_health(
                &lease,
                "/matrix/exhausted",
                &exhausted_analysis(upstream_status),
                None,
            )
            .await
            .expect("reconcile exhausted");
        assert_eq!(
            effect.code.as_str(),
            KEY_EFFECT_MARKED_EXHAUSTED,
            "{provider}"
        );
        assert_eq!(key_status(&proxy, &secret).await, "exhausted", "{provider}");
        assert_eq!(
            maintenance_rows(&proxy, &key_id, MAINTENANCE_OP_AUTO_MARK_EXHAUSTED).await,
            1,
            "{provider} audit row"
        );
    }
    // M8 binding negative: the new-provider flows above wrote ZERO depletion
    // rows despite the seeded threshold crossing.
    assert_eq!(
        depletion_rows(&proxy).await,
        0,
        "M8: new providers never write api_key_low_quota_depletions"
    );
}

/// Tavily-432 positive control: the untouched 432 gate still records a
/// depletion row for a Tavily key at/below the threshold (proves the M8
/// negative assertions above can actually fire).
#[tokio::test]
async fn tavily_432_positive_control_still_records_depletion() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-tavily-432").await;
    let key_id = seed_provider_key(&proxy, ProviderId::Tavily, "mtx-tvly-432").await;
    sqlx::query("UPDATE api_keys SET quota_remaining = 0 WHERE id = ?")
        .bind(&key_id)
        .execute(&proxy.key_store.pool)
        .await
        .expect("seed zero remaining");
    let lease = proxy
        .key_store
        .acquire_key_for_provider(ProviderId::Tavily)
        .await
        .expect("tavily lease");
    let effect = proxy
        .reconcile_key_health(&lease, "/matrix/tavily-432", &exhausted_analysis(432), None)
        .await
        .expect("reconcile tavily 432");
    assert_eq!(effect.code.as_str(), KEY_EFFECT_MARKED_EXHAUSTED);
    assert_eq!(
        depletion_rows(&proxy).await,
        1,
        "Tavily 432 keeps its depletion behavior byte-stable"
    );
}

/// M8 gate unit pin: the generalized depletion predicate refuses every
/// non-Tavily provider even on a literal 432 status.
#[test]
fn low_quota_depletion_gate_refuses_new_providers_even_on_432() {
    for provider in [ProviderId::Serpapi, ProviderId::Exa, ProviderId::Brave] {
        assert!(
            !low_quota_depletion_applies(provider, Some(432)),
            "{provider} must never write depletion rows"
        );
    }
    assert!(low_quota_depletion_applies(ProviderId::Tavily, Some(432)));
    assert!(!low_quota_depletion_applies(ProviderId::Tavily, Some(402)));
}

/// AuthFailed per provider: quarantine row mirroring the Tavily auth-fail
/// shape (reason invalid_api_key, cleared_at NULL) + audit maintenance row.
#[tokio::test]
async fn auth_failed_signal_quarantines_with_maintenance_row_per_provider() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-authfail").await;
    // Provider auth-fail statuses: serpapi 401, exa 401, brave 422
    // SUBSCRIPTION_TOKEN_INVALID (live-probed classifier vocabulary).
    for (provider, upstream_status) in [
        (ProviderId::Serpapi, 401i64),
        (ProviderId::Exa, 401),
        (ProviderId::Brave, 422),
    ] {
        let secret = format!("mtx-af-{}", provider.as_str());
        let key_id = seed_provider_key(&proxy, provider, &secret).await;
        let lease = proxy
            .key_store
            .acquire_key_for_provider(provider)
            .await
            .expect("lease");
        let effect = proxy
            .reconcile_key_health(
                &lease,
                "/matrix/authfail",
                &auth_failed_analysis(upstream_status),
                None,
            )
            .await
            .expect("reconcile auth failure");
        assert_eq!(effect.code.as_str(), KEY_EFFECT_QUARANTINED, "{provider}");
        assert_eq!(
            active_quarantine_reason(&proxy, &key_id).await.as_deref(),
            Some(BLOCKED_KEY_REASON_INVALID_API_KEY),
            "{provider} quarantine row"
        );
        assert_eq!(
            maintenance_rows(&proxy, &key_id, MAINTENANCE_OP_AUTO_QUARANTINE).await,
            1,
            "{provider} audit row"
        );
    }
    assert_eq!(depletion_rows(&proxy).await, 0, "M8 stays negative");
}

/// Success-restore per provider (Exa SuccessOnly recovery completes here):
/// an exhausted key flips back to active with its audit row + effect.
#[tokio::test]
async fn success_restores_exhausted_keys_with_maintenance_row_per_provider() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-restore").await;
    for provider in [ProviderId::Serpapi, ProviderId::Exa, ProviderId::Brave] {
        let secret = format!("mtx-sr-{}", provider.as_str());
        let key_id = seed_provider_key(&proxy, provider, &secret).await;
        assert!(
            proxy
                .key_store
                .mark_quota_exhausted(&secret)
                .await
                .expect("seed exhausted")
        );
        // Tier-2 fallback re-leases the exhausted key (house semantics).
        let lease = proxy
            .key_store
            .acquire_key_for_provider(provider)
            .await
            .expect("exhausted lease");
        assert_eq!(lease.id, key_id);
        let effect = proxy
            .reconcile_key_health(&lease, "/matrix/success", &success_analysis(), None)
            .await
            .expect("reconcile success");
        assert_eq!(
            effect.code.as_str(),
            KEY_EFFECT_RESTORED_ACTIVE,
            "{provider}"
        );
        assert_eq!(key_status(&proxy, &secret).await, "active", "{provider}");
        assert_eq!(
            maintenance_rows(&proxy, &key_id, MAINTENANCE_OP_AUTO_RESTORE_ACTIVE).await,
            1,
            "{provider} audit row"
        );
    }
    assert_eq!(depletion_rows(&proxy).await, 0, "M8 stays negative");
}

// ---------------------------------------------------------------------------
// Alert events: provider field flow (existing type keys, zero new kinds).
// ---------------------------------------------------------------------------

/// Billing-settle-shaped `auth_token_logs` seed for the alert matrix (the
/// facade settle path writes these rows with the provider stamp + failure
/// kind projection).
#[derive(Clone, Copy)]
struct AtlAlertSeed<'a> {
    token_id: &'a str,
    key_id: Option<&'a str>,
    provider: Option<&'a str>,
    result_status: &'a str,
    failure_kind: Option<&'a str>,
    http_status: i64,
    path: &'a str,
    kind_key: Option<&'a str>,
    created_at: i64,
}

async fn seed_alert_atl_row(proxy: &TavilyProxy, seed: AtlAlertSeed<'_>) {
    sqlx::query(
        "INSERT INTO auth_token_logs (
            token_id, method, path, query, http_status, mcp_status,
            request_kind_key, request_kind_label, result_status, failure_kind,
            key_effect_code, binding_effect_code, selection_effect_code,
            provider, api_key_id, counts_business_quota, created_at
         ) VALUES (?, 'GET', ?, NULL, ?, NULL, ?, ?, ?, ?, 'none', 'none', 'none', ?, ?, 1, ?)",
    )
    .bind(seed.token_id)
    .bind(seed.path)
    .bind(seed.http_status)
    .bind(seed.kind_key)
    .bind(seed.kind_key.map(|key| format!("label {key}")))
    .bind(seed.result_status)
    .bind(seed.failure_kind)
    .bind(seed.provider)
    .bind(seed.key_id)
    .bind(seed.created_at)
    .execute(&proxy.key_store.pool)
    .await
    .expect("seed atl alert row");
}

/// Wire-shape provider extraction: reads the SERIALIZED event so the probe
/// pins the exact API/payload surface (a missing field is `None`, and legacy
/// rows must never gain the key).
fn wire_provider(event: &AlertEventRecord) -> Option<String> {
    serde_json::to_value(event)
        .expect("serialize alert event")
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// The full alert matrix: real reconcile-written maintenance rows (serpapi
/// quarantine, exa exhaustion) + facade-shaped atl rows (brave 429, tavily
/// 429/432 controls) → alert events keyed by EXISTING type constants, each
/// new-provider event carrying its provider and every Tavily/legacy event
/// serializing WITHOUT the provider key (byte-stable wire JSON). Both read
/// paths are covered: raw CTE before the projection catches up and the
/// projected read model after.
#[tokio::test]
async fn alert_events_carry_provider_for_new_providers_and_keep_tavily_wire_shape() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-alerts").await;
    let now = proxy.backend_time.now_ts();
    sqlx::query(
        "INSERT INTO auth_tokens (id, secret, created_at) VALUES ('hmx-token', 'hmx-secret', ?)",
    )
    .bind(now)
    .execute(&proxy.key_store.pool)
    .await
    .expect("seed token");

    // SerpApi auth-fail quarantine via the real reconcile path.
    let serpapi_key_id = seed_provider_key(&proxy, ProviderId::Serpapi, "mtx-al-srp").await;
    let serpapi_lease = proxy
        .key_store
        .acquire_key_for_provider(ProviderId::Serpapi)
        .await
        .expect("serpapi lease");
    proxy
        .reconcile_key_health(
            &serpapi_lease,
            "/matrix/alerts/serpapi",
            &auth_failed_analysis(401),
            Some("hmx-token"),
        )
        .await
        .expect("serpapi quarantine");

    // Exa exhaustion via the real reconcile path.
    let exa_key_id = seed_provider_key(&proxy, ProviderId::Exa, "mtx-al-exa").await;
    let exa_lease = proxy
        .key_store
        .acquire_key_for_provider(ProviderId::Exa)
        .await
        .expect("exa lease");
    proxy
        .reconcile_key_health(
            &exa_lease,
            "/matrix/alerts/exa",
            &exhausted_analysis(402),
            Some("hmx-token"),
        )
        .await
        .expect("exa exhaustion");

    // Brave 429 + Tavily controls as billing-settle-shaped atl rows.
    let brave_key_id = seed_provider_key(&proxy, ProviderId::Brave, "mtx-al-brv").await;
    seed_alert_atl_row(
        &proxy,
        AtlAlertSeed {
            token_id: "hmx-token",
            key_id: Some(&brave_key_id),
            provider: Some("brave"),
            result_status: OUTCOME_ERROR,
            failure_kind: Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
            http_status: 429,
            path: "/api/brave/res/v1/web/search",
            kind_key: Some("api:brave/web"),
            created_at: now,
        },
    )
    .await;
    let tavily_key_id = seed_provider_key(&proxy, ProviderId::Tavily, "mtx-al-tvly").await;
    seed_alert_atl_row(
        &proxy,
        AtlAlertSeed {
            token_id: "hmx-token",
            key_id: Some(&tavily_key_id),
            provider: None,
            result_status: OUTCOME_ERROR,
            failure_kind: Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
            http_status: 429,
            path: "/api/tavily/search",
            kind_key: Some("api:search"),
            created_at: now - 1,
        },
    )
    .await;
    seed_alert_atl_row(
        &proxy,
        AtlAlertSeed {
            token_id: "hmx-token",
            key_id: Some(&tavily_key_id),
            provider: None,
            result_status: OUTCOME_QUOTA_EXHAUSTED,
            failure_kind: None,
            http_status: 432,
            path: "/api/tavily/search",
            kind_key: Some("api:search"),
            created_at: now - 2,
        },
    )
    .await;

    // READ PATH 1 — raw CTE (fresh source rows are ahead of the projection
    // watermark, so the page serves from the live branch).
    let raw_page = proxy
        .key_store
        .fetch_alert_events_page(None, None, None, None, None, None, &[], 1, 100)
        .await
        .expect("raw alert page");
    let summarize = |items: &[AlertEventRecord]| -> Vec<(String, Option<String>, String)> {
        let mut rows = items
            .iter()
            .map(|event| {
                (
                    event.alert_type.clone(),
                    wire_provider(event),
                    event.subject_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    let raw_rows = summarize(&raw_page.items);
    assert!(
        raw_rows.contains(&(
            ALERT_TYPE_UPSTREAM_KEY_BLOCKED.to_string(),
            Some("serpapi".to_string()),
            serpapi_key_id.clone()
        )),
        "serpapi quarantine alert carries provider (raw): {raw_rows:?}"
    );
    assert!(
        raw_rows.contains(&(
            ALERT_TYPE_API_KEY_EXHAUSTED.to_string(),
            Some("exa".to_string()),
            exa_key_id.clone()
        )),
        "exa exhaustion alert carries provider (raw): {raw_rows:?}"
    );
    assert!(
        raw_rows.contains(&(
            ALERT_TYPE_UPSTREAM_RATE_LIMITED_429.to_string(),
            Some("brave".to_string()),
            brave_key_id.clone()
        )),
        "brave 429 alert carries provider (raw): {raw_rows:?}"
    );
    // Tavily controls: existing types, NO provider on the record or the wire.
    let tavily_429 = raw_page
        .items
        .iter()
        .find(|event| {
            event.alert_type == ALERT_TYPE_UPSTREAM_RATE_LIMITED_429
                && event.subject_id == tavily_key_id
        })
        .expect("tavily 429 alert");
    assert!(wire_provider(tavily_429).is_none());
    let tavily_wire = serde_json::to_value(tavily_429).expect("serialize tavily event");
    assert!(
        tavily_wire.get("provider").is_none(),
        "legacy Tavily wire JSON must not gain a provider key (byte-stable)"
    );
    let tavily_432 = raw_page
        .items
        .iter()
        .find(|event| event.alert_type == ALERT_TYPE_UPSTREAM_USAGE_LIMIT_432)
        .expect("tavily 432 alert (branch stays Tavily-scoped)");
    assert!(wire_provider(tavily_432).is_none());
    assert!(
        raw_page
            .items
            .iter()
            .any(|event| wire_provider(event).as_deref() == Some("serpapi")),
        "serpapi event exposes provider on the wire"
    );

    // Projection write side: advance until the seeded events land, then pin
    // payload_json provider round-trip + absence for legacy rows.
    let mut projected_events = 0i64;
    for _ in 0..48 {
        proxy
            .advance_dashboard_alert_projection_slice()
            .await
            .expect("advance projection slice");
        projected_events = sqlx::query_scalar(
            "SELECT COUNT(*) FROM observability.dashboard_alert_projection_events",
        )
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("count projected events");
        if projected_events >= 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        projected_events >= 5,
        "projection converged with the seeded events"
    );
    let payloads: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM observability.dashboard_alert_projection_events",
    )
    .fetch_all(&proxy.key_store.pool)
    .await
    .expect("payload dump");
    assert!(
        payloads
            .iter()
            .any(|payload| payload.contains("\"provider\":\"serpapi\"")),
        "serpapi provider persisted in the projection payload"
    );
    assert!(
        payloads
            .iter()
            .any(|payload| payload.contains("\"provider\":\"exa\"")),
        "exa provider persisted in the projection payload"
    );
    assert!(
        payloads
            .iter()
            .any(|payload| payload.contains("\"provider\":\"brave\"")),
        "brave provider persisted in the projection payload"
    );
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(ALERT_TYPE_UPSTREAM_USAGE_LIMIT_432) && !payload.contains("provider")
        }),
        "legacy tavily payload keeps its byte shape (no provider key)"
    );

    // READ PATH 2 — projected read model after convergence.
    let projected_page = proxy
        .key_store
        .fetch_alert_events_page(None, None, None, None, None, None, &[], 1, 100)
        .await
        .expect("projected alert page");
    let projected_rows = summarize(&projected_page.items);
    assert_eq!(
        projected_rows, raw_rows,
        "raw and projected read paths expose the identical provider flow"
    );
}

// ---------------------------------------------------------------------------
// FU-A: brave registered full-path alias kinds (additive).
// ---------------------------------------------------------------------------

/// The three ADDITIVE full-path aliases classify the registered native brave
/// paths onto the canonical kinds/labels, and the canonical short paths stay
/// unchanged (Tavily + siblings byte-stable).
#[test]
fn brave_full_path_aliases_classify_registered_kinds() {
    for (full_path, canonical_path, kind, label) in [
        (
            "/api/brave/res/v1/web/search",
            "/api/brave/web/search",
            "api:brave/web",
            "API | brave/web",
        ),
        (
            "/api/brave/res/v1/news/search",
            "/api/brave/news/search",
            "api:brave/news",
            "API | brave/news",
        ),
        (
            "/api/brave/res/v1/llm/context",
            "/api/brave/llm/context",
            "api:brave/llm-context",
            "API | brave/llm-context",
        ),
    ] {
        let full = classify_token_request_kind(full_path, None);
        assert_eq!(full.key, kind, "full path {full_path}");
        assert_eq!(full.label, label);
        let canonical = classify_token_request_kind(canonical_path, None);
        assert_eq!(canonical.key, kind, "canonical path stays {canonical_path}");
        assert_eq!(canonical.label, label);
    }
}

/// SQL-mirror pin: provider rows pass through the stored-key whitelist for
/// BOTH brave path shapes (the trigger/CTE SQL never derives provider kinds
/// from paths, so the aliases need no SQL-side sibling).
#[tokio::test]
async fn brave_full_path_sql_kind_whitelist_pins() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("memory pool");
    for path in ["'/api/brave/res/v1/web/search'", "'/api/brave/web/search'"] {
        let stored: String = sqlx::query_scalar(&format!(
            "SELECT {} AS v",
            token_log_request_kind_key_sql(path, "'api:brave/web'")
        ))
        .fetch_one(&pool)
        .await
        .expect("stored-kind sql executes");
        assert_eq!(stored, "api:brave/web", "path {path}");
        let request_log: String = sqlx::query_scalar(&format!(
            "SELECT {} AS v",
            request_log_request_kind_key_sql(path, "NULL", "'api:brave/web'")
        ))
        .fetch_one(&pool)
        .await
        .expect("request-log kind sql executes");
        assert_eq!(request_log, "api:brave/web", "path {path}");
    }
}

// ---------------------------------------------------------------------------
// Plan provider-key-429-retry todo 3 — auth-block family LIVE matrix rows:
// bare 401/403 → classify → arm → selector avoidance, plus the negative rows
// that keep the family's longer ceiling from leaking outside it.
// ---------------------------------------------------------------------------

/// One scripted answer from the stub upstream (a local listener only — these
/// rows never touch a production endpoint).
struct MatrixUpstreamRecipe {
    status: StatusCode,
    retry_after: Option<&'static str>,
    body: &'static [u8],
}

/// The `api_key` secrets the stub upstream saw, in hit order: the observable
/// record of WHICH pooled key the live selector handed to each attempt.
type MatrixUpstreamHits = Arc<std::sync::Mutex<Vec<String>>>;

fn matrix_upstream_hits() -> MatrixUpstreamHits {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

fn matrix_hits_seen(hits: &MatrixUpstreamHits) -> Vec<String> {
    hits.lock().expect("upstream hit log lock").clone()
}

/// Stub Tavily upstream: `/search` answers from the recipe list (the last
/// recipe repeats for any extra hit) and `/usage` keeps the quota-sync lane
/// resolvable should the executor reach it.
async fn spawn_matrix_upstream(
    recipes: Vec<MatrixUpstreamRecipe>,
    hits: MatrixUpstreamHits,
) -> std::net::SocketAddr {
    let recipes = Arc::new(recipes);
    let app = Router::new()
        .route(
            "/search",
            post(move |body: axum::body::Bytes| {
                let recipes = Arc::clone(&recipes);
                let hits = Arc::clone(&hits);
                async move {
                    let api_key = serde_json::from_slice::<Value>(body.as_ref())
                        .ok()
                        .and_then(|parsed| {
                            parsed
                                .get("api_key")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    let index = {
                        let mut seen = hits.lock().expect("upstream hit log lock");
                        seen.push(api_key);
                        seen.len() - 1
                    };
                    let recipe = &recipes[index.min(recipes.len() - 1)];
                    let mut response =
                        axum::response::Response::new(axum::body::Body::from(recipe.body.to_vec()));
                    *response.status_mut() = recipe.status;
                    if let Some(retry_after) = recipe.retry_after {
                        response.headers_mut().insert(
                            axum::http::header::RETRY_AFTER,
                            axum::http::HeaderValue::from_str(retry_after)
                                .expect("retry-after header value"),
                        );
                    }
                    response
                }
            }),
        )
        .route(
            "/usage",
            get(|| async {
                Json(serde_json::json!({
                    "key": { "limit": 1000, "usage": 1 },
                    "account": { "plan_limit": 1000, "usage": 1 },
                }))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub upstream");
    let addr = listener.local_addr().expect("stub upstream address");
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve stub upstream");
    });
    addr
}

/// Seeds every Tavily key in ONE `sync_keys` call: the sync soft-deletes
/// same-provider keys absent from the list, so seeding one key at a time would
/// drop the pool member the avoidance rows depend on.
async fn seed_tavily_matrix_keys(proxy: &TavilyProxy, secrets: &[&str]) -> Vec<String> {
    let owned = secrets
        .iter()
        .map(|secret| (*secret).to_string())
        .collect::<Vec<_>>();
    proxy
        .key_store
        .sync_keys(ProviderId::Tavily, &owned)
        .await
        .expect("seed tavily matrix keys");
    let mut ids = Vec::with_capacity(owned.len());
    for secret in &owned {
        ids.push(
            sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ?")
                .bind(secret)
                .fetch_one(&proxy.key_store.pool)
                .await
                .expect("matrix key id lookup"),
        );
    }
    ids
}

async fn seed_matrix_auth_token(proxy: &TavilyProxy, token_id: &str) {
    sqlx::query("INSERT INTO auth_tokens (id, secret, created_at) VALUES (?, ?, ?)")
        .bind(token_id)
        .bind(format!("{token_id}-secret"))
        .bind(proxy.backend_time.now_ts())
        .execute(&proxy.key_store.pool)
        .await
        .expect("seed matrix auth token");
}

/// One live Tavily HTTP search through the production executor: key selection →
/// upstream call → classification → reconcile → backoff arm → request_logs.
/// This is the entry point `/api/tavily/search` calls, so the rows below pin
/// the auth-block family end to end instead of pinning the store call alone.
async fn matrix_live_search(
    proxy: &TavilyProxy,
    usage_base: &str,
    token_id: &str,
) -> (ProxyResponse, AttemptAnalysis) {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    proxy
        .proxy_http_json_endpoint(
            usage_base,
            "/search",
            Some(token_id),
            false,
            None,
            None,
            &Method::POST,
            "/api/tavily/search",
            serde_json::json!({ "query": "matrix auth block" }),
            &headers,
            false,
            None,
        )
        .await
        .expect("live matrix search completes")
}

/// The single `(key_id, scope)` backoff row — reason, absolute deadline and
/// sized Retry-After — whether or not its cooldown has elapsed.
async fn matrix_backoff_row(
    proxy: &TavilyProxy,
    key_id: &str,
    scope: &str,
) -> Option<(String, i64, i64)> {
    sqlx::query(
        "SELECT reason_code, cooldown_until, retry_after_secs FROM api_key_transient_backoffs \
         WHERE key_id = ? AND scope = ?",
    )
    .bind(key_id)
    .bind(scope)
    .fetch_optional(&proxy.key_store.pool)
    .await
    .expect("matrix backoff row lookup")
    .map(|row| {
        (
            row.get::<Option<String>, _>("reason_code")
                .unwrap_or_default(),
            row.get::<i64, _>("cooldown_until"),
            row.get::<Option<i64>, _>("retry_after_secs").unwrap_or(-1),
        )
    })
}

async fn matrix_backoff_row_count(proxy: &TavilyProxy, key_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM api_key_transient_backoffs WHERE key_id = ?")
        .bind(key_id)
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("matrix backoff row count")
}

/// Visible `request_logs` rows for one key, oldest first:
/// `(status_code, failure_kind, key_effect_code)`.
async fn matrix_request_log_rows(
    proxy: &TavilyProxy,
    key_id: &str,
) -> Vec<(Option<i64>, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT status_code, failure_kind, key_effect_code FROM request_logs \
         WHERE api_key_id = ? ORDER BY id ASC",
    )
    .bind(key_id)
    .fetch_all(&proxy.key_store.pool)
    .await
    .expect("matrix request log rows")
}
/// LIVE end-to-end auth-block row (plan todo 3a): a bare Tavily 403 — no
/// quarantine keyword anywhere in the body — travels the production executor,
/// arms `upstream_auth_blocked` on the shared `http_global` scope with the
/// upstream's own Retry-After (600s, above the old 300s ladder ceiling), keeps
/// the key active and unquarantined, and the NEXT acquisition hands out the
/// other pooled key instead.
///
/// The `tavily_` name prefix is deliberate, not cosmetic: libtest collects (and
/// therefore starts) this shard's tests in NAME order, so the three proxy +
/// SQLite + stub-HTTP rows below sort last and never share a two-thread window
/// with the pre-existing `alert_events_*` row. That row's `admin_alerts_read`
/// runs on a 250ms wall-clock admission budget (`src/store/sqlite_runtime.rs:34`)
/// which turns even a successful query into `Deferred{reason:"read_budget"}`
/// once the deadline passes, so a heavy neighbour alone is enough to fail it.
#[tokio::test]
async fn tavily_auth_block_bare_403_live_path_arms_and_next_acquisition_avoids_key() {
    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-auth-block-live").await;
    let secrets = ["mtx-ab-live-a", "mtx-ab-live-b"];
    let key_ids = seed_tavily_matrix_keys(&proxy, &secrets).await;
    let token_id = "hmx-ab-live-token";
    seed_matrix_auth_token(&proxy, token_id).await;
    let hits = matrix_upstream_hits();
    let addr = spawn_matrix_upstream(
        vec![
            MatrixUpstreamRecipe {
                status: StatusCode::FORBIDDEN,
                retry_after: Some("600"),
                body: br#"{"detail":"Forbidden"}"#,
            },
            MatrixUpstreamRecipe {
                status: StatusCode::OK,
                retry_after: None,
                body: br#"{"status":200,"results":[]}"#,
            },
        ],
        Arc::clone(&hits),
    )
    .await;
    let usage_base = format!("http://{addr}");
    let now = proxy.backend_time.now_ts();

    let (blocked_resp, blocked_analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
    assert_eq!(blocked_resp.status, StatusCode::FORBIDDEN);
    assert_eq!(blocked_analysis.status, OUTCOME_ERROR);
    assert_eq!(blocked_analysis.tavily_status_code, Some(403));
    assert_eq!(
        blocked_analysis.failure_kind.as_deref(),
        Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED),
        "a bare 403 classifies into the auth-block family"
    );
    assert_eq!(blocked_analysis.key_health_action, KeyHealthAction::None);
    assert_eq!(
        blocked_resp.key_effect_code,
        KEY_EFFECT_TRANSIENT_BACKOFF_SET
    );
    assert_eq!(
        blocked_resp.body.as_ref(),
        br#"{"detail":"Forbidden"}"#.as_slice(),
        "the rejected upstream body still passes through byte-identical"
    );

    let seen = matrix_hits_seen(&hits);
    assert_eq!(seen.len(), 1, "one upstream attempt");
    let blocked_index = secrets
        .iter()
        .position(|secret| *secret == seen[0])
        .expect("the leased key is a pooled key");
    let blocked_id = key_ids[blocked_index].clone();
    assert_eq!(
        blocked_resp.api_key_id.as_deref(),
        Some(blocked_id.as_str())
    );

    let (reason, cooldown_until, retry_after) =
        matrix_backoff_row(&proxy, &blocked_id, HTTP_GLOBAL_BACKOFF_SCOPE)
            .await
            .expect("auth-block backoff armed");
    assert_eq!(reason, FAILURE_KIND_UPSTREAM_AUTH_BLOCKED);
    assert_eq!(
        retry_after, 600,
        "RA=600 survives the family's [30, 900] clamp (todo 2)"
    );
    assert_eq!(cooldown_until, now + 600);
    assert_eq!(
        key_status(&proxy, &seen[0]).await,
        "active",
        "cooling never exhausts the key"
    );
    assert!(
        active_quarantine_reason(&proxy, &blocked_id)
            .await
            .is_none(),
        "no quarantine keyword → no quarantine row"
    );
    assert_eq!(
        matrix_request_log_rows(&proxy, &blocked_id).await,
        vec![(
            Some(403),
            Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string()),
            Some(KEY_EFFECT_TRANSIENT_BACKOFF_SET.to_string())
        )],
        "the blocked attempt stays visible with its family label"
    );
    std::eprintln!(
        "[auth-block-matrix] live-403 key={blocked_id} reason={reason} cooldown_secs={} \
         retry_after={retry_after} effect={} upstream_hits={seen:?}",
        cooldown_until - now,
        blocked_resp.key_effect_code,
    );

    // Phase 2 — the next acquisition routes around the cooled key.
    let (healthy_resp, healthy_analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
    let seen = matrix_hits_seen(&hits);
    assert_eq!(
        seen.len(),
        2,
        "the retry-free executor still makes one attempt"
    );
    assert_ne!(
        seen[1], seen[0],
        "the selector avoids the cooled key on the next acquisition"
    );
    let healthy_index = secrets
        .iter()
        .position(|secret| *secret == seen[1])
        .expect("the leased key is a pooled key");
    assert_ne!(healthy_index, blocked_index);
    let healthy_id = key_ids[healthy_index].clone();
    assert_eq!(
        healthy_resp.api_key_id.as_deref(),
        Some(healthy_id.as_str())
    );
    assert_eq!(healthy_resp.status, StatusCode::OK);
    assert_eq!(healthy_analysis.status, OUTCOME_SUCCESS);
    assert!(
        matrix_backoff_row(&proxy, &healthy_id, HTTP_GLOBAL_BACKOFF_SCOPE)
            .await
            .is_none(),
        "the healthy key never enters the family"
    );
    let (reason_after, cooldown_after, retry_after_after) =
        matrix_backoff_row(&proxy, &blocked_id, HTTP_GLOBAL_BACKOFF_SCOPE)
            .await
            .expect("the armed row survives");
    assert_eq!(
        (reason_after.as_str(), cooldown_after, retry_after_after),
        (FAILURE_KIND_UPSTREAM_AUTH_BLOCKED, now + 600, 600),
        "another key's success does not clear the cooled key's row"
    );
    std::eprintln!(
        "[auth-block-matrix] live-403 avoidance blocked={blocked_id} -> next={healthy_id} \
         second_status={} remaining_cooldown_secs={} healthy_backoff_rows=0",
        healthy_resp.status.as_u16(),
        cooldown_after - now,
    );
}

/// One LIVE row of the auth-block family boundary. `expected_reason` and
/// `expected_secs` describe the `http_global` backoff the row must arm; both
/// `None` means the row must arm NOTHING on any scope.
struct MatrixFamilyRow {
    label: &'static str,
    status: StatusCode,
    retry_after: Option<&'static str>,
    body: &'static [u8],
    expected_kind: &'static str,
    expected_effect: &'static str,
    expected_reason: Option<&'static str>,
    expected_secs: Option<i64>,
    expect_quarantine: bool,
}

/// LIVE family-boundary rows (plan todo 3, malformed_input + 防误扩 probes),
/// table-driven over ONE shared proxy, ONE shared stub upstream and ONE token —
/// the fixture count is the minimum that keeps the coverage:
/// * what ENTERS the family: a bare 401 with an EMPTY body and a bare 403 with
///   an invalid-UTF-8 body both classify as `upstream_auth_blocked`, never
///   quarantine, and arm the sized cooldown (120s base with no Retry-After,
///   900s for RA=1200 through the lifted ceiling). Family membership keys off
///   the effective status, so every malformed-body shape shares one code path;
///   the 30s floor and the remaining Retry-After fixtures stay pinned by the
///   pure sizing table
///   `unknown_403_auth_block_backoff_honors_retry_after_up_to_the_900s_ceiling`.
/// * what must NOT enter it: a keyword 403 quarantines with ZERO backoff rows
///   on any scope, a 500 arms nothing even with RA=600 on the wire, and a 429
///   keeps the rate-limit ladder (RA=600 clamps to 300, not the family's 900).
/// Each row lands on a fresh pooled key, so the run re-proves selector
/// avoidance row by row. ROW ORDER IS LOAD-BEARING: the token's primary
/// affinity re-serves a still-eligible key on the next acquisition, so every
/// row except the last must leave its key ineligible (cooled or quarantined) —
/// the one row that arms nothing (`upstream-500`) therefore runs last.
#[tokio::test]
async fn tavily_auth_block_family_boundary_live_rows() {
    let rows = [
        MatrixFamilyRow {
            label: "bare-401-empty-body",
            status: StatusCode::UNAUTHORIZED,
            retry_after: None,
            body: b"",
            expected_kind: FAILURE_KIND_UPSTREAM_AUTH_BLOCKED,
            expected_effect: KEY_EFFECT_TRANSIENT_BACKOFF_SET,
            expected_reason: Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED),
            expected_secs: Some(120),
            expect_quarantine: false,
        },
        MatrixFamilyRow {
            label: "bare-403-invalid-utf8-ra-above-ceiling",
            status: StatusCode::FORBIDDEN,
            retry_after: Some("1200"),
            body: b"\xff\xfe\x00\x80 garbage",
            expected_kind: FAILURE_KIND_UPSTREAM_AUTH_BLOCKED,
            expected_effect: KEY_EFFECT_TRANSIENT_BACKOFF_SET,
            expected_reason: Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED),
            expected_secs: Some(900),
            expect_quarantine: false,
        },
        MatrixFamilyRow {
            label: "keyword-403-quarantine",
            status: StatusCode::FORBIDDEN,
            retry_after: Some("600"),
            body: br#"{"detail":{"error":"The account associated with this API key has been deactivated."}}"#,
            expected_kind: FAILURE_KIND_UPSTREAM_ACCOUNT_DEACTIVATED_401,
            expected_effect: KEY_EFFECT_QUARANTINED,
            expected_reason: None,
            expected_secs: None,
            expect_quarantine: true,
        },
        MatrixFamilyRow {
            label: "rate-limited-429",
            status: StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some("600"),
            body: br#"{"detail":"Excessive requests"}"#,
            expected_kind: FAILURE_KIND_UPSTREAM_RATE_LIMITED_429,
            expected_effect: KEY_EFFECT_TRANSIENT_BACKOFF_SET,
            expected_reason: Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
            expected_secs: Some(300),
            expect_quarantine: false,
        },
        // LAST on purpose: this is the only row that arms nothing, so its key
        // stays eligible and the affinity lane would hand the SAME key to a
        // following row.
        MatrixFamilyRow {
            label: "upstream-500",
            status: StatusCode::INTERNAL_SERVER_ERROR,
            retry_after: Some("600"),
            body: br#"{"detail":"upstream boom"}"#,
            expected_kind: FAILURE_KIND_OTHER,
            expected_effect: KEY_EFFECT_NONE,
            expected_reason: None,
            expected_secs: None,
            expect_quarantine: false,
        },
    ];

    let (proxy, _clock) = spawn_matrix_proxy("health-matrix-auth-block-boundary").await;
    let secrets = [
        "mtx-ab-b1",
        "mtx-ab-b2",
        "mtx-ab-b3",
        "mtx-ab-b4",
        "mtx-ab-b5",
    ];
    let key_ids = seed_tavily_matrix_keys(&proxy, &secrets).await;
    let token_id = "hmx-ab-boundary-token";
    seed_matrix_auth_token(&proxy, token_id).await;
    let hits = matrix_upstream_hits();
    let recipes = rows
        .iter()
        .map(|row| MatrixUpstreamRecipe {
            status: row.status,
            retry_after: row.retry_after,
            body: row.body,
        })
        .collect::<Vec<_>>();
    let addr = spawn_matrix_upstream(recipes, Arc::clone(&hits)).await;
    let usage_base = format!("http://{addr}");

    let mut used_secrets: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let label = row.label;
        let before = proxy.backend_time.now_ts();
        let (resp, analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
        assert_eq!(resp.status, row.status, "{label}");
        assert_eq!(analysis.status, OUTCOME_ERROR, "{label}");
        assert_eq!(
            analysis.failure_kind.as_deref(),
            Some(row.expected_kind),
            "{label}"
        );
        assert_eq!(resp.key_effect_code, row.expected_effect, "{label}");

        let seen = matrix_hits_seen(&hits);
        assert_eq!(seen.len(), index + 1, "{label}: one upstream attempt");
        let secret = seen[index].clone();
        assert!(
            !used_secrets.contains(&secret),
            "{label}: every earlier row left its key cooled or quarantined, so the selector \
             must hand out a fresh one"
        );
        used_secrets.push(secret.clone());
        let key_index = secrets
            .iter()
            .position(|candidate| *candidate == secret)
            .expect("the leased key is a pooled key");
        let key_id = key_ids[key_index].clone();
        assert_eq!(resp.api_key_id.as_deref(), Some(key_id.as_str()), "{label}");

        let armed = matrix_backoff_row(&proxy, &key_id, HTTP_GLOBAL_BACKOFF_SCOPE).await;
        if let (Some(reason), Some(secs)) = (row.expected_reason, row.expected_secs) {
            let (armed_reason, cooldown_until, sized) = armed
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: backoff armed"));
            assert_eq!(armed_reason.as_str(), reason, "{label}");
            if reason == FAILURE_KIND_UPSTREAM_AUTH_BLOCKED {
                assert_eq!(
                    analysis.key_health_action,
                    KeyHealthAction::None,
                    "{label}: a bare 401/403 cools, it never quarantines"
                );
            } else {
                assert_ne!(
                    armed_reason.as_str(),
                    FAILURE_KIND_UPSTREAM_AUTH_BLOCKED,
                    "{label}: the family code must not leak onto another signal"
                );
            }
            assert_eq!(*sized, secs, "{label}");
            assert_eq!(*cooldown_until, before + secs, "{label}");
        } else {
            assert!(armed.is_none(), "{label}: no http_global row");
            assert_eq!(
                matrix_backoff_row_count(&proxy, &key_id).await,
                0,
                "{label}: nothing armed on any scope"
            );
        }
        if row.expect_quarantine {
            assert_eq!(
                active_quarantine_reason(&proxy, &key_id).await.as_deref(),
                Some("account_deactivated"),
                "{label}"
            );
        } else {
            assert!(
                active_quarantine_reason(&proxy, &key_id).await.is_none(),
                "{label}"
            );
            assert_eq!(key_status(&proxy, &secret).await, "active", "{label}");
        }
        assert_eq!(
            matrix_request_log_rows(&proxy, &key_id).await,
            vec![(
                Some(i64::from(row.status.as_u16())),
                Some(row.expected_kind.to_string()),
                Some(row.expected_effect.to_string())
            )],
            "{label}: one visible row"
        );
        std::eprintln!(
            "[auth-block-matrix] boundary {label} status={} retry_after={:?} -> kind={} \
             effect={} armed={armed:?} quarantine={}",
            row.status.as_u16(),
            row.retry_after,
            row.expected_kind,
            row.expected_effect,
            row.expect_quarantine,
        );
    }
    assert_eq!(
        depletion_rows(&proxy).await,
        0,
        "neither the family nor its neighbours write depletion rows"
    );
}

/// LIVE repeat-arm row (plan todo 3b, stale-state probe): three consecutive
/// bare 403s on a single-key pool re-enter classification and the arm path
/// every time, so the store's MAX(cooldown_until) upsert is exercised through
/// the executor rather than by a direct store call — a shorter later hint keeps
/// the longer armed deadline, a longer one extends it. The backend clock is
/// ADVANCED between hits, so the extension is an absolute-deadline move, and
/// the effect labels pin the arm's own report: only an EXTENDING arm re-labels
/// the attempt `transient_backoff_set`.
#[tokio::test]
async fn tavily_auth_block_repeat_live_hits_extend_and_never_shorten_cooldown() {
    let (proxy, clock) = spawn_matrix_proxy("health-matrix-auth-block-rearm").await;
    let secrets = ["mtx-ab-rearm-only"];
    let key_ids = seed_tavily_matrix_keys(&proxy, &secrets).await;
    let key_id = key_ids.first().expect("seeded key").clone();
    let token_id = "hmx-ab-rearm-token";
    seed_matrix_auth_token(&proxy, token_id).await;
    let hits = matrix_upstream_hits();
    let blocked_body: &'static [u8] = br#"{"detail":"Forbidden"}"#;
    let addr = spawn_matrix_upstream(
        vec![
            MatrixUpstreamRecipe {
                status: StatusCode::FORBIDDEN,
                retry_after: Some("600"),
                body: blocked_body,
            },
            MatrixUpstreamRecipe {
                status: StatusCode::FORBIDDEN,
                retry_after: None,
                body: blocked_body,
            },
            MatrixUpstreamRecipe {
                status: StatusCode::FORBIDDEN,
                retry_after: Some("900"),
                body: blocked_body,
            },
        ],
        Arc::clone(&hits),
    )
    .await;
    let usage_base = format!("http://{addr}");
    let t0 = proxy.backend_time.now_ts();

    // Hit 1 — RA=600 arms the family's full 600s (todo 2's lifted ceiling).
    let (first_resp, first_analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
    assert_eq!(
        first_analysis.failure_kind.as_deref(),
        Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED)
    );
    assert_eq!(first_resp.key_effect_code, KEY_EFFECT_TRANSIENT_BACKOFF_SET);
    let first_row = matrix_backoff_row(&proxy, &key_id, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("armed after hit 1");
    assert_eq!(
        first_row,
        (
            FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string(),
            t0 + 600,
            600
        )
    );

    // Hit 2 — 300s later with NO Retry-After: sizing asks for t0+300+120,
    // earlier than the armed t0+600, so the deadline must not move.
    clock.advance_wall(Duration::from_secs(300));
    let t1 = proxy.backend_time.now_ts();
    assert_eq!(t1, t0 + 300);
    let (second_resp, second_analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
    assert_eq!(
        second_analysis.failure_kind.as_deref(),
        Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED),
        "the repeat hit re-enters the family"
    );
    assert_eq!(
        second_resp.key_effect_code, KEY_EFFECT_NONE,
        "a non-extending re-arm reports no NEW cooldown, so the label stays none"
    );
    let second_row = matrix_backoff_row(&proxy, &key_id, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("still armed after hit 2");
    assert_eq!(
        second_row, first_row,
        "a shorter re-arm never shortens the armed cooldown"
    );
    assert_eq!(
        second_row.1 - t1,
        300,
        "300s of the original cooldown remain"
    );

    // Hit 3 — 60s later with RA=900: t0+360+900 beats the armed deadline.
    clock.advance_wall(Duration::from_secs(60));
    let t2 = proxy.backend_time.now_ts();
    assert_eq!(t2, t0 + 360);
    let (third_resp, third_analysis) = matrix_live_search(&proxy, &usage_base, token_id).await;
    assert_eq!(
        third_analysis.failure_kind.as_deref(),
        Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED)
    );
    assert_eq!(third_resp.key_effect_code, KEY_EFFECT_TRANSIENT_BACKOFF_SET);
    let third_row = matrix_backoff_row(&proxy, &key_id, HTTP_GLOBAL_BACKOFF_SCOPE)
        .await
        .expect("still armed after hit 3");
    assert_eq!(
        third_row,
        (
            FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string(),
            t2 + 900,
            900
        ),
        "a longer later hint extends the absolute deadline"
    );
    assert!(
        third_row.1 > second_row.1,
        "the cooldown grew, never shrank"
    );

    assert_eq!(
        matrix_hits_seen(&hits),
        vec![secrets[0].to_string(); 3],
        "the single pooled key is re-leased for every repeat hit"
    );
    assert_eq!(
        matrix_request_log_rows(&proxy, &key_id).await,
        vec![
            (
                Some(403),
                Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string()),
                Some(KEY_EFFECT_TRANSIENT_BACKOFF_SET.to_string())
            ),
            (
                Some(403),
                Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string()),
                Some(KEY_EFFECT_NONE.to_string())
            ),
            (
                Some(403),
                Some(FAILURE_KIND_UPSTREAM_AUTH_BLOCKED.to_string()),
                Some(KEY_EFFECT_TRANSIENT_BACKOFF_SET.to_string())
            ),
        ],
        "every repeat hit stays visible with its family label"
    );
    assert_eq!(
        key_status(&proxy, secrets[0]).await,
        "active",
        "repeat blocks cool the key, they never exhaust it"
    );
    assert!(active_quarantine_reason(&proxy, &key_id).await.is_none());
    assert_eq!(depletion_rows(&proxy).await, 0);
    std::eprintln!(
        "[auth-block-matrix] live-rearm key={key_id} hits=3 deadline=[t0+{}, t0+{} (kept), \
         t0+{} (extended)] sized=[{}, {}, {}] effects=[set, none, set]",
        first_row.1 - t0,
        second_row.1 - t0,
        third_row.1 - t0,
        first_row.2,
        second_row.2,
        third_row.2,
    );
}
