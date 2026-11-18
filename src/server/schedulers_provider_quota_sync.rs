// SerpApi `/account` + Zhihu `/api/v1/quota` quota-sync lanes (plan todo 19;
// keenable-integration todo 12; amendment M4).
//
// ONE job-type string per lane (`quota_sync/serpapi`, `quota_sync/zhihu`,
// mirroring `quota_sync/` manual + hot) riding the EXISTING quota-sync
// scheduler loops and the EXISTING maintenance-worker deque path — never a
// new scheduler loop, never an unbounded lane, never a new interval (cold
// 24h / hot 2h+15m pending thresholds are reused verbatim). Remote work goes
// through the single shared remote-attempt lease
// (REMOTE_IO_SCHEDULED_JOB_TYPES registry entries), so Tavily-/usage and the
// provider lanes contend for ONE outbound slot (M4 C4a). Keenable has NO
// lane: its balance is passive-only and is never polled.
//
// The gate mirrors `sync_key_quota_with_db_job_gate` geometry: DB reads
// under the maintenance read gate, remote fetch outside DB gates (inside
// the lease + QUOTA_SYNC_JOB_TIMEOUT_SECS envelope), durable writes under
// the job-execution gate. Provider guards fail CLOSED before any egress:
// each lane is single-provider (a Tavily key can never be sent to SerpApi or
// Zhihu), exactly as the Tavily lane is tavily-only (M4 zero-leak).

const QUOTA_SYNC_SERPAPI_JOB_TYPE: &str = "quota_sync/serpapi";

/// The one Zhihu lane job type (keenable-integration todo 12). Rides the
/// existing quota-sync sweep loops and maintenance-worker deque path.
const QUOTA_SYNC_ZHIHU_JOB_TYPE: &str = "quota_sync/zhihu";

/// The ONE dispatch arm body for `quota_sync/zhihu` (keenable-integration
/// todo 12). Mirrors the SerpApi lane gate: provider guard fails CLOSED
/// before admission/egress, the free `/api/v1/quota` fetch rides the shared
/// remote-attempt lease + per-key forward-proxy plan, and the durable write
/// happens under the job-execution + maintenance gates.
async fn sync_zhihu_key_quota_with_db_job_gate(
    state: &AppState,
    key_id: &str,
    manual_remote_attempt: bool,
) -> Result<(tavily_hikari::ZhihuQuotaSnapshot, tavily_hikari::KeyEffect), ProxyError> {
    let (secret, provider) = {
        let _maintenance = acquire_db_maintenance_read_gate().await;
        let secret = state.proxy.quota_sync_api_key_secret(key_id).await?;
        let provider = state.proxy.quota_sync_key_provider(key_id).await?;
        (secret, provider)
    };
    if provider != Some(tavily_hikari::providers::ProviderId::Zhihu) {
        return Err(ProxyError::Other(format!(
            "provider_guard_rejected: quota_sync/zhihu lane is zhihu-only, key provider is {}",
            provider
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string())
        )));
    }
    let admission = remote_attempt_admission_for_state(state);
    let result = tokio::time::timeout(
        Duration::from_secs(QUOTA_SYNC_JOB_TIMEOUT_SECS),
        state.proxy.fetch_zhihu_quota_for_sync(
            &secret,
            key_id,
            admission,
            manual_remote_attempt,
            Duration::from_secs(tavily_hikari::QUOTA_SYNC_FETCH_TIMEOUT_SECS),
        ),
    )
    .await;
    let snapshot = match result {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(err)) => return Err(err),
        Err(_) => {
            return Err(ProxyError::Other(format!(
                "quota_sync/zhihu timed out after {}s",
                QUOTA_SYNC_JOB_TIMEOUT_SECS
            )));
        }
    };
    let _job_execution_gate = acquire_db_job_execution_gate_for_state(state).await;
    let _maintenance = acquire_db_maintenance_read_gate().await;
    let effect = state
        .proxy
        .record_zhihu_quota_sync(key_id, &snapshot, QUOTA_SYNC_ZHIHU_JOB_TYPE)
        .await?;
    Ok((snapshot, effect))
}

async fn finish_zhihu_quota_sync_job(
    state: &AppState,
    job_id: i64,
    claim_generation: i64,
    status: &'static str,
    message: &str,
) -> bool {
    let _ = state
        .proxy
        .scheduled_job_finish_claimed(job_id, claim_generation, status, Some(message))
        .await;
    status == "success"
}

async fn run_zhihu_quota_sync_claimed_job(
    state: &Arc<AppState>,
    key_id: Option<String>,
    job_id: i64,
    claim_generation: i64,
    manual_remote_attempt: bool,
) -> bool {
    let Some(key_id) = key_id else {
        return finish_zhihu_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            "missing key_id",
        )
        .await;
    };
    match sync_zhihu_key_quota_with_db_job_gate(state, &key_id, manual_remote_attempt).await {
        Ok((snapshot, effect)) => finish_zhihu_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "success",
            &format!(
                "limit={} remaining={} renews_at={} effect={}",
                snapshot.quota_limit,
                snapshot.quota_remaining,
                snapshot.quota_renews_at,
                effect.code
            ),
        )
        .await,
        Err(ProxyError::QuotaDataMissing { reason }) => finish_zhihu_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            &format!("quota_data_missing: {reason}"),
        )
        .await,
        Err(ProxyError::UsageHttp { status, body }) => finish_zhihu_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            &format!("usage_http {status}: {body}"),
        )
        .await,
        Err(err) => {
            finish_zhihu_quota_sync_job(state, job_id, claim_generation, "error", &err.to_string())
                .await
        }
    }
}

async fn sync_serpapi_key_quota_with_db_job_gate(
    state: &AppState,
    key_id: &str,
    manual_remote_attempt: bool,
) -> Result<(i64, i64, tavily_hikari::SerpapiAccountSyncEffects), ProxyError> {
    let (secret, provider) = {
        let _maintenance = acquire_db_maintenance_read_gate().await;
        let secret = state.proxy.quota_sync_api_key_secret(key_id).await?;
        let provider = state.proxy.quota_sync_key_provider(key_id).await?;
        (secret, provider)
    };
    if provider != Some(tavily_hikari::providers::ProviderId::Serpapi) {
        // Fail CLOSED before admission/egress (M4 symmetry with the Tavily
        // lane guard): the /account lane must never carry a foreign secret.
        return Err(ProxyError::Other(format!(
            "provider_guard_rejected: quota_sync/serpapi lane is serpapi-only, key provider is {}",
            provider
                .map(|value| value.to_string())
                .unwrap_or_else(|| "missing".to_string())
        )));
    }
    let admission = remote_attempt_admission_for_state(state);
    let result = tokio::time::timeout(
        Duration::from_secs(QUOTA_SYNC_JOB_TIMEOUT_SECS),
        state
            .proxy
            .fetch_serpapi_account_for_sync(&secret, key_id, admission, manual_remote_attempt),
    )
    .await;
    // Error path note: the Tavily lane's record_quota_sync_usage_error is
    // the /api/tavily/usage quarantine machinery and deliberately NOT
    // reused here — a failed /account sync surfaces as a job error whose
    // job_failed alert inherits provider='serpapi' automatically (T18).
    let snapshot = match result {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(err)) => return Err(err),
        Err(_) => {
            return Err(ProxyError::Other(format!(
                "quota_sync/serpapi timed out after {}s",
                QUOTA_SYNC_JOB_TIMEOUT_SECS
            )));
        }
    };
    let _job_execution_gate = acquire_db_job_execution_gate_for_state(state).await;
    let _maintenance = acquire_db_maintenance_read_gate().await;
    let effects = state
        .proxy
        .record_serpapi_account_sync(key_id, &snapshot, QUOTA_SYNC_SERPAPI_JOB_TYPE)
        .await?;
    Ok((snapshot.quota_limit, snapshot.quota_remaining, effects))
}

async fn finish_serpapi_quota_sync_job(
    state: &AppState,
    job_id: i64,
    claim_generation: i64,
    status: &'static str,
    message: &str,
) -> bool {
    let _ = state
        .proxy
        .scheduled_job_finish_claimed(job_id, claim_generation, status, Some(message))
        .await;
    status == "success"
}

/// The ONE dispatch arm body for `quota_sync/serpapi` (dequeued through the
/// existing maintenance worker; manual triggers reuse it via manual_remote_
/// attempt exactly like the Tavily quota_sync family).
async fn run_serpapi_quota_sync_claimed_job(
    state: &Arc<AppState>,
    key_id: Option<String>,
    job_id: i64,
    claim_generation: i64,
    manual_remote_attempt: bool,
) -> bool {
    let Some(key_id) = key_id else {
        return finish_serpapi_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            "missing key_id",
        )
        .await;
    };
    match sync_serpapi_key_quota_with_db_job_gate(state, &key_id, manual_remote_attempt).await {
        Ok((limit, remaining, effects)) => finish_serpapi_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "success",
            &format!(
                "limit={limit} remaining={remaining} renewed_to_active={} account_note_recorded={}",
                effects.renewed_to_active, effects.account_note_recorded
            ),
        )
        .await,
        Err(ProxyError::QuotaDataMissing { reason }) => finish_serpapi_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            &format!("quota_data_missing: {reason}"),
        )
        .await,
        Err(ProxyError::UsageHttp { status, body }) => finish_serpapi_quota_sync_job(
            state,
            job_id,
            claim_generation,
            "error",
            &format!("usage_http {status}: {body}"),
        )
        .await,
        Err(err) => {
            finish_serpapi_quota_sync_job(state, job_id, claim_generation, "error", &err.to_string())
                .await
        }
    }
}

/// Per-sweep provider lane body, invoked from BOTH existing quota-sync
/// loops (`cold` = 24h threshold sweep; hot = 2h-active/15m-stale sweep).
/// Same pacing discipline as the Tavily lanes: bounded list under the
/// maintenance read gate, randomized jitter before each enqueue, dedupe
/// via the durable (job_type, key_id) active identity, and the SAME
/// `PER_SWEEP_ENQUEUE_CAP` bound so a backlog drains across sweeps.
async fn run_provider_pending_quota_sync_lane(
    state: &AppState,
    provider: tavily_hikari::providers::ProviderId,
    job_type: &'static str,
    log_prefix: &'static str,
    cold: bool,
    jitter_max_inclusive: u64,
) {
    let scope = if cold { "cold" } else { "hot" };
    let keys = {
        let _maintenance = acquire_db_maintenance_read_gate().await;
        let listed = if cold {
            state
                .proxy
                .list_keys_pending_provider_quota_sync(provider, twenty_four_hours_secs())
                .await
        } else {
            state
                .proxy
                .list_keys_pending_hot_provider_quota_sync(
                    provider,
                    two_hours_secs(),
                    fifteen_minutes_secs(),
                )
                .await
        };
        match listed {
            Ok(list) => list,
            Err(err) => {
                tracing::warn!(
                    component = "quota_sync",
                    event = "list_pending_failed",
                    scope = scope,
                    provider = %provider,
                    err = %err,
                );
                vec![]
            }
        }
    };

    // Bounded like the Tavily lanes — a restart backlog drains across the
    // following sweeps instead of one burst.
    for key_id in keys.into_iter().take(PER_SWEEP_ENQUEUE_CAP) {
        let delay = random_delay_secs(jitter_max_inclusive);
        state
            .proxy
            .backend_time()
            .sleep(Duration::from_secs(delay))
            .await;
        let Some(_) = enqueue_scheduled_job_logged(
            state,
            job_type,
            Some(&key_id),
            TRIGGER_SOURCE_SCHEDULER,
            log_prefix,
        )
        .await
        else {
            continue;
        };
    }
}

/// Per-sweep serpapi lane body (plan todo 19).
async fn run_serpapi_pending_quota_sync_lane(
    state: &AppState,
    cold: bool,
    jitter_max_inclusive: u64,
) {
    run_provider_pending_quota_sync_lane(
        state,
        tavily_hikari::providers::ProviderId::Serpapi,
        QUOTA_SYNC_SERPAPI_JOB_TYPE,
        "quota-sync-serpapi",
        cold,
        jitter_max_inclusive,
    )
    .await;
}

/// Per-sweep Zhihu lane body (keenable-integration todo 12).
async fn run_zhihu_pending_quota_sync_lane(
    state: &AppState,
    cold: bool,
    jitter_max_inclusive: u64,
) {
    run_provider_pending_quota_sync_lane(
        state,
        tavily_hikari::providers::ProviderId::Zhihu,
        QUOTA_SYNC_ZHIHU_JOB_TYPE,
        "quota-sync-zhihu",
        cold,
        jitter_max_inclusive,
    )
    .await;
}
