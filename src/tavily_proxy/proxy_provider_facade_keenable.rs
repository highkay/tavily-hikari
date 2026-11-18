//! Keenable REST facade execution layer (keenable-integration todo 7).
//!
//! Layered over the generic seam (`proxy_provider_rest_facade`):
//! * upstream-response classification via the committed todo-3 keenable
//!   classifier, projected into the shared `AttemptAnalysis` contract so the
//!   EXISTING `reconcile_key_health` machinery applies the acceptance health
//!   actions (401 / credentials_forbidden auth-fail -> quarantine row, 402
//!   billing_error/insufficient_credits -> exhausted, 429 -> transient backoff
//!   on the shared HTTP scope sized by the generic Retry-After lane until
//!   todo 11 lands the keenable-specific sizing; target_error/target_blocked
//!   and every other client-shaped envelope NEVER marks the key);
//! * request_logs persistence (provider stamp, legacy `tavily_status_code`
//!   column carries the upstream status, bodies per retention profile with
//!   `redact_api_key_bytes` secret redaction - the pooled X-API-Key and th-
//!   tokens are never persisted; rows key on the canonical kind path
//!   `/api/keenable/{search,fetch}` so the store-derived request_kind matches
//!   the bin-boundary kind);
//! * the PASSIVE CREDIT HARVEST: every success body carrying
//!   `usage.credits_remaining` is persisted ONLY on a material change
//!   through `KeyStore::record_quota_sync_sample_without_limit` with the
//!   distinct source `keenable_passive`. The harvest adds ZERO outbound
//!   requests (it reads the already-buffered response body), never fails the
//!   request (best-effort, contention-skipped), leaves the live key's
//!   `quota_limit` NULL and never touches `quota_renews_at` (a prepaid
//!   balance has no plan limit and no calendar reset);
//! * a deterministic bin-side outcome projection (`keenable_facade_outcome`)
//!   carrying `business_success` (true ONLY on `ProviderFailureSignal::Ok`):
//!   the classifier can auth-fail on a 2xx body (`authentication_error` rides
//!   the body, not the status), so the billing settlement gates on the
//!   signal, never on HTTP 2xx alone. A 402/429/5xx never bills and a 403
//!   target_blocked never marks the key.
//!
//! The POST `/v1/search` body passes through byte-for-byte (the client
//! `X-API-Key` header slot is stripped at the server boundary before the
//! executor; Keenable carries no body auth slot) and the GET `/v1/fetch`
//! query string forwards unchanged. The response body is never transformed
//! beyond redaction.

use super::*;

/// Passive-harvest source stamp (DISTINCT value; never reused by the
/// Tavily/SerpApi/Brave sync paths so the sample table stays auditable per
/// source).
const KEENABLE_PASSIVE_QUOTA_SYNC_SOURCE: &str = "keenable_passive";

/// Deterministic projection of one Keenable facade response onto the
/// token-log vocabulary (result status + failure kind) plus the billing gate.
/// Exported for the bin boundary where the classifier internals are
/// crate-private.
#[derive(Debug, Clone, Copy)]
pub struct KeenableFacadeOutcome {
    /// `success` / `quota_exhausted` / `error` (shared OUTCOME_* vocabulary).
    pub result_status: &'static str,
    pub failure_kind: Option<&'static str>,
    /// Billing gate (keenable-integration todo 7): true ONLY on
    /// `ProviderFailureSignal::Ok`. The keenable envelope carries `type` in
    /// the BODY, so an HTTP 2xx can still be an auth/billing/target failure -
    /// business success is the classifier signal, never the transport status.
    pub business_success: bool,
}

/// Classify a buffered Keenable upstream response (todo-3 classifier) without
/// header evidence; Retry-After sizing is handled lib-side where the response
/// headers are live.
pub fn keenable_facade_outcome(status: StatusCode, body: &[u8]) -> KeenableFacadeOutcome {
    let signal = classify_provider_failure(
        providers::ProviderId::Keenable,
        &ProviderFailureEvidence {
            status: status.as_u16(),
            headers: None,
            body,
        },
    );
    KeenableFacadeOutcome {
        result_status: keenable_result_status(signal),
        failure_kind: signal.failure_kind(),
        business_success: signal == ProviderFailureSignal::Ok,
    }
}

fn keenable_result_status(signal: ProviderFailureSignal) -> &'static str {
    match signal {
        ProviderFailureSignal::Ok => OUTCOME_SUCCESS,
        ProviderFailureSignal::Exhausted => OUTCOME_QUOTA_EXHAUSTED,
        ProviderFailureSignal::AuthFailed
        | ProviderFailureSignal::RateLimited { .. }
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => OUTCOME_ERROR,
    }
}

fn keenable_key_health_action(
    status: StatusCode,
    body: &[u8],
    signal: ProviderFailureSignal,
) -> KeyHealthAction {
    match signal {
        // 402 billing_error codes (insufficient_credits per the todo-3
        // classifier) exhaust the key; recovery belongs to the UtcMonth
        // reset_monthly lane (todo 11) or a manual restore - never to this
        // record path.
        ProviderFailureSignal::Exhausted => KeyHealthAction::MarkExhausted,
        // Auth-fail feeds the EXISTING quarantine machinery (mirrors the
        // SerpApi/Exa/Tavily invalid-key behavior). M8: quarantine/exhaustion
        // for new providers never writes api_key_low_quota_depletions.
        ProviderFailureSignal::AuthFailed => KeyHealthAction::Quarantine(QuarantineDecision {
            reason_code: BLOCKED_KEY_REASON_INVALID_API_KEY.to_string(),
            reason_summary: format!(
                "Keenable rejected the pooled API key as invalid (HTTP {})",
                status.as_u16()
            ),
            reason_detail: truncate_text(
                String::from_utf8_lossy(body).trim(),
                QUARANTINE_REASON_DETAIL_MAX_LEN,
            ),
        }),
        // target_error/target_blocked, malformed client requests, 429
        // throughput and 5xx transients are the CALLER's or the UPSTREAM's
        // problem - never a reason to mark the pooled key.
        ProviderFailureSignal::Ok
        | ProviderFailureSignal::RateLimited { .. }
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => KeyHealthAction::None,
    }
}

fn keenable_attempt_analysis(
    status: StatusCode,
    body: &[u8],
    signal: ProviderFailureSignal,
) -> AttemptAnalysis {
    AttemptAnalysis {
        status: keenable_result_status(signal),
        // The legacy column carries the upstream HTTP status verbatim for
        // provider rows (keenable-integration todo 7 logging contract).
        tavily_status_code: Some(i64::from(status.as_u16())),
        key_health_action: keenable_key_health_action(status, body, signal),
        failure_kind: signal.failure_kind().map(str::to_string),
        key_effect: KeyEffect::none(),
        api_key_id: None,
    }
}

/// Parse the passive credit balance out of a keyed Keenable response body
/// (`usage.credits_remaining`; SPEC: may lag seconds, may be absent - an
/// absent or non-numeric value is simply no sample, never an error). Numeric
/// strings are accepted because envelopes in this family have been observed
/// stringifying counters.
fn parse_keenable_credits_remaining(body: &[u8]) -> Option<i64> {
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let credits = parsed.get("usage")?.get("credits_remaining")?;
    keenable_credits_value(credits)
}

/// Numeric coercion shared by the REST (`usage.credits_remaining`) and hosted
/// MCP (`_meta["keenable/usage"].credits_remaining`) passive parsers:
/// numbers, and numeric strings because envelopes in this family have been
/// observed stringifying counters.
fn keenable_credits_value(credits: &serde_json::Value) -> Option<i64> {
    match credits {
        serde_json::Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Parse the passive credit balance out of a buffered hosted-MCP JSON-RPC
/// response body (keenable-integration todo 8): a single response exposes it
/// at `result._meta["keenable/usage"].credits_remaining`; a batch exposes it
/// per result and the LAST observed value wins (each tool result carries the
/// balance as of that call). Absent `_meta` (every free method, every error
/// envelope) is simply no sample, never an error.
fn parse_keenable_mcp_credits_remaining(body: &[u8]) -> Option<i64> {
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let credits_of = |message: &serde_json::Value| {
        message
            .get("result")?
            .get("_meta")?
            .get("keenable/usage")?
            .get("credits_remaining")
            .and_then(keenable_credits_value)
    };
    match parsed {
        serde_json::Value::Object(_) => credits_of(&parsed),
        serde_json::Value::Array(items) => items.iter().filter_map(credits_of).next_back(),
        _ => None,
    }
}

impl TavilyProxy {
    /// Post-response bookkeeping for the Keenable REST facade (todo-7 mirror
    /// of `record_exa_rest_facade_attempt` + the passive credit harvest):
    /// reconcile, transient backoff arming, request_logs persistence,
    /// backoff/log linking, and the best-effort credit-balance harvest. The
    /// SerpApi/Exa/Brave record paths stay byte-identical behind their own
    /// gates; Tavily REST never reaches this seam and Zhihu keeps the generic
    /// recorder until todo 9.
    pub(crate) async fn record_keenable_rest_facade_attempt(
        &self,
        ctx: &ProviderFacadeAttemptContext<'_>,
    ) -> Result<ProviderFacadeAttemptRecord, ProxyError> {
        if ctx.request.provider != providers::ProviderId::Keenable {
            return Ok(ProviderFacadeAttemptRecord::boundary_default());
        }

        let analysis = keenable_attempt_analysis(ctx.status, ctx.body_bytes, ctx.signal);
        let mut key_effect = self
            .reconcile_key_health(
                ctx.lease,
                ctx.request.kind_path,
                &analysis,
                ctx.request.auth_token_id.as_deref(),
            )
            .await?;
        if key_effect.code == KEY_EFFECT_NONE && analysis.status == OUTCOME_SUCCESS {
            key_effect = self
                .clear_transient_backoffs_after_success(
                    &ctx.lease.id,
                    ctx.request.kind_path,
                    ctx.request.auth_token_id.as_deref(),
                )
                .await?;
        }

        // 429 rate_limit_error (todo-3 RateLimited - Keenable has no
        // exhaustion-flavored 429): arm the shared HTTP backoff scope that
        // provider-scoped acquisition avoids. Sizing runs the generic
        // Retry-After lane (Retry-After / retry_after_seconds through the
        // shared ladder clamp) until todo 11 lands the keenable-specific
        // second-scale sizing on the same scope.
        let armed_backoff = match ctx.signal {
            ProviderFailureSignal::RateLimited { .. } => {
                let now = self.backend_time.now_ts();
                match Self::effective_provider_rate_limited_backoff_secs(
                    ctx.request.provider,
                    ctx.signal,
                    now,
                ) {
                    Some(retry_after_secs) => self
                        .key_store
                        .arm_api_key_transient_backoff(ApiKeyTransientBackoffArm {
                            key_id: &ctx.lease.id,
                            scope: HTTP_GLOBAL_BACKOFF_SCOPE,
                            cooldown_until: now + retry_after_secs,
                            retry_after_secs,
                            reason_code: Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
                            source_request_log_id: None,
                            now,
                        })
                        .await?
                        .is_some(),
                    // Unreachable for RateLimited; keep the match exhaustive
                    // without an escape hatch.
                    None => false,
                }
            }
            ProviderFailureSignal::Ok
            | ProviderFailureSignal::Exhausted
            | ProviderFailureSignal::AuthFailed
            | ProviderFailureSignal::ClientError
            | ProviderFailureSignal::ServerTransient => false,
        };
        if armed_backoff && key_effect.code == KEY_EFFECT_NONE {
            key_effect = Self::transient_backoff_set_effect();
        }

        // Secrets redacted before persistence (M17): the pooled key rides the
        // X-API-Key header only and the client header slot was stripped at the
        // boundary, so the persisted bodies/query never carry th- tokens or
        // provider keys; the redaction pass stays as the defense-in-depth
        // backstop. The POST body itself forwards byte-for-byte (todo 7
        // contract: no body transformation beyond this redaction).
        let redacted_request_body = redact_api_key_bytes(&ctx.request.body);
        let redacted_response_body = redact_api_key_bytes(ctx.body_bytes);
        let request_log_id = self
            .key_store
            .log_attempt(AttemptLog {
                key_id: Some(&ctx.lease.id),
                auth_token_id: ctx.request.auth_token_id.as_deref(),
                method: &ctx.request.method,
                // Canonical kind path (todo-3 static table entry), same
                // request_logs keying rule as the Brave record path: the store
                // derives request_kind_key (and the value-bucket / retention
                // bucketing) from this column, and the canonical short form
                // maps to api:keenable/{search,fetch}. The client-facing full
                // /v1 mirror path stays on auth_token_logs (bin boundary).
                path: ctx.request.kind_path,
                query: ctx.request.query.as_deref(),
                status: Some(ctx.status),
                tavily_status_code: analysis.tavily_status_code,
                error: None,
                request_body: &redacted_request_body,
                response_body: &redacted_response_body,
                outcome: analysis.status,
                failure_kind: analysis.failure_kind.as_deref(),
                key_effect_code: key_effect.code.as_str(),
                key_effect_summary: key_effect.summary.as_deref(),
                binding_effect_code: KEY_EFFECT_NONE,
                binding_effect_summary: None,
                selection_effect_code: KEY_EFFECT_NONE,
                selection_effect_summary: None,
                gateway_mode: None,
                experiment_variant: None,
                provider: Some(ctx.request.provider.as_str()),
                proxy_session_id: None,
                routing_subject_hash: None,
                upstream_operation: Some(ctx.request.upstream_operation),
                fallback_reason: ctx
                    .retry_marker
                    .fallback_reason(analysis.status == OUTCOME_SUCCESS),
                forwarded_headers: &ctx.sanitized_headers.forwarded,
                dropped_headers: &ctx.sanitized_headers.dropped,
                client_ip: ctx.request.client_ip.as_ref(),
            })
            .await?;
        self.link_transient_backoff_clear_request_log(&key_effect, &ctx.lease.id, request_log_id)
            .await?;
        if armed_backoff {
            self.key_store
                .set_api_key_transient_backoff_request_log_id(
                    &ctx.lease.id,
                    HTTP_GLOBAL_BACKOFF_SCOPE,
                    request_log_id,
                    self.backend_time.now_ts(),
                )
                .await?;
        }

        // Passive credit harvest: best-effort, bounded, material-only, and
        // NEVER fails the request. Runs after request_logs so the audit row is
        // durable first; a fast no-op when the response carries no
        // usage.credits_remaining (every error envelope), so it never extends
        // the failure-path latency and adds zero outbound requests.
        self.harvest_keenable_passive_quota(&ctx.lease.id, ctx.body_bytes)
            .await;

        tracing::debug!(
            component = "provider_facade",
            event = "provider_facade_attempt_recorded",
            provider = %ctx.request.provider,
            operation = ctx.request.upstream_operation,
            status = ctx.status.as_u16(),
            result_status = analysis.status,
            failure_kind = ?analysis.failure_kind.as_deref(),
            key_effect = %key_effect.code,
            api_key_id = %ctx.lease.id,
            request_log_id,
            "provider facade attempt recorded"
        );

        Ok(ProviderFacadeAttemptRecord {
            request_log_id: Some(request_log_id),
            key_effect,
        })
    }

    /// Passive credit harvest (keenable-integration todo 7). Parses
    /// `usage.credits_remaining` from the ALREADY-BUFFERED response body
    /// (zero extra requests) and, only on a material change, persists
    /// `quota_remaining` / `quota_synced_at` through the no-limit
    /// quota-sample seam with source `keenable_passive`. The live key keeps
    /// `quota_limit` NULL and `quota_renews_at` untouched. Best-effort: a
    /// missing balance returns before any DB read (bounding failure-path
    /// latency), and any read/write error is logged at debug and swallowed so
    /// the harvest NEVER fails the request or holds the response.
    async fn harvest_keenable_passive_quota(&self, key_id: &str, body: &[u8]) {
        let Some(credits_remaining) = parse_keenable_credits_remaining(body) else {
            // No balance advertised (every error envelope, or a partial
            // success body): no DB read, no write. Keeps the failure path fast
            // and honors the SPEC's "may be absent".
            return;
        };
        self.persist_keenable_passive_credits(key_id, credits_remaining)
            .await;
    }

    /// Passive credit harvest for the hosted-MCP passthrough (keenable-integration
    /// todo 8): reads `_meta["keenable/usage"].credits_remaining` from the
    /// ALREADY-BUFFERED JSON-RPC response body (zero extra requests) and shares
    /// the REST harvest's material-change persistence seam (source
    /// `keenable_passive`, quota_limit NULL, quota_renews_at untouched,
    /// best-effort). The caller is the server-side passthrough pipeline.
    pub async fn harvest_keenable_mcp_passive_quota(&self, key_id: &str, mcp_body: &[u8]) {
        let Some(credits_remaining) = parse_keenable_mcp_credits_remaining(mcp_body) else {
            // No balance advertised (every free method and every error
            // envelope): no DB read, no write.
            return;
        };
        self.persist_keenable_passive_credits(key_id, credits_remaining)
            .await;
    }

    /// Material-change persistence shared by the REST and hosted-MCP passive
    /// harvests (todo-7 contract, unchanged semantics).
    async fn persist_keenable_passive_credits(&self, key_id: &str, credits_remaining: i64) {
        // Bounded foreground read of the stored balance to gate the material
        // change. On error treat as absent (best-effort) - a redundant write
        // is idempotent-valued and harmless; contention never blocks the
        // response.
        let stored: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT quota_remaining FROM api_keys WHERE id = ? LIMIT 1",
        )
        .bind(key_id)
        .fetch_optional(&self.key_store.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(None);
        if stored == Some(credits_remaining) {
            // Non-material repeat (bounded-write proof): identical balance, no
            // sample row, no api_keys write.
            return;
        }

        let now = self.backend_time.now_ts();
        if let Err(err) = self
            .key_store
            .record_quota_sync_sample_without_limit(
                key_id,
                credits_remaining,
                now,
                KEENABLE_PASSIVE_QUOTA_SYNC_SOURCE,
            )
            .await
        {
            tracing::debug!(
                component = "provider_facade",
                event = "keenable_passive_quota_harvest_deferred",
                api_key_id = %key_id,
                err = %err,
                "keenable passive quota harvest skipped (best-effort, contention-safe)"
            );
        }
    }

    /// Transport-failure audit row for the Keenable REST facade (mirrors the
    /// Exa/Brave transport-error rows). No response body exists on a
    /// transport failure (including the todo-4 30s timeout expiry), so there
    /// is nothing to harvest. `fallback_reason` is the retry-chain marker
    /// of this dead attempt.
    pub(crate) async fn log_keenable_rest_facade_transport_error(
        &self,
        request: &ProviderRestFacadeRequest,
        lease: &ApiKeyLease,
        sanitized_headers: &SanitizedHeaders,
        err: &ProxyError,
        fallback_reason: Option<&'static str>,
    ) -> Result<(), ProxyError> {
        if request.provider != providers::ProviderId::Keenable {
            return Ok(());
        }
        let redacted_request_body = redact_api_key_bytes(&request.body);
        let redacted_empty: Vec<u8> = Vec::new();
        let error_message = err.to_string();
        self.key_store
            .log_attempt(AttemptLog {
                key_id: Some(&lease.id),
                auth_token_id: request.auth_token_id.as_deref(),
                method: &request.method,
                // Canonical kind path - same request_logs keying rule as the
                // success record path (store derives request_kind from it).
                path: request.kind_path,
                query: request.query.as_deref(),
                status: None,
                tavily_status_code: None,
                error: Some(&error_message),
                request_body: &redacted_request_body,
                response_body: &redacted_empty,
                outcome: OUTCOME_ERROR,
                failure_kind: failure_kind_from_proxy_error(err),
                key_effect_code: KEY_EFFECT_NONE,
                key_effect_summary: None,
                binding_effect_code: KEY_EFFECT_NONE,
                binding_effect_summary: None,
                selection_effect_code: KEY_EFFECT_NONE,
                selection_effect_summary: None,
                gateway_mode: None,
                experiment_variant: None,
                provider: Some(request.provider.as_str()),
                proxy_session_id: None,
                routing_subject_hash: None,
                upstream_operation: Some(request.upstream_operation),
                fallback_reason,
                forwarded_headers: &sanitized_headers.forwarded,
                dropped_headers: &sanitized_headers.dropped,
                client_ip: request.client_ip.as_ref(),
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod keenable_facade_translation_tests {
    use super::*;

    #[test]
    fn keenable_outcome_projects_classifier_signals_onto_token_log_vocabulary() {
        let ok = keenable_facade_outcome(
            StatusCode::OK,
            br#"{"query":"mock","results":[],"usage":{"credits_remaining":99999}}"#,
        );
        assert_eq!(ok.result_status, OUTCOME_SUCCESS);
        assert_eq!(ok.failure_kind, None);
        assert!(ok.business_success, "a plain 200 success bills");

        // 402 insufficient_credits -> exhausted, never billed.
        let exhausted = keenable_facade_outcome(
            StatusCode::PAYMENT_REQUIRED,
            br#"{"error":"Insufficient credits","type":"billing_error","code":"insufficient_credits"}"#,
        );
        assert_eq!(exhausted.result_status, OUTCOME_QUOTA_EXHAUSTED);
        assert_eq!(exhausted.failure_kind, None);
        assert!(!exhausted.business_success);

        // 403 target_blocked -> client error (NOT auth-fail, NOT exhausted) -
        // never marks the key, never bills.
        let target = keenable_facade_outcome(
            StatusCode::FORBIDDEN,
            br#"{"error":"Upstream forbidden","type":"target_error","code":"target_blocked"}"#,
        );
        assert_eq!(target.result_status, OUTCOME_ERROR);
        assert_eq!(target.failure_kind, Some(FAILURE_KIND_OTHER));
        assert!(!target.business_success);

        // 401 authentication_error -> auth-fail error vocabulary.
        let auth = keenable_facade_outcome(
            StatusCode::UNAUTHORIZED,
            br#"{"error":"Authentication failed","type":"authentication_error","code":"invalid_api_key"}"#,
        );
        assert_eq!(auth.result_status, OUTCOME_ERROR);
        assert_eq!(
            auth.failure_kind,
            Some(FAILURE_KIND_UPSTREAM_ACCOUNT_DEACTIVATED_401)
        );
        assert!(!auth.business_success);

        // 429 rate_limit_error -> rate-limited vocabulary.
        let throttled = keenable_facade_outcome(
            StatusCode::TOO_MANY_REQUESTS,
            br#"{"error":"Rate limit exceeded","type":"rate_limit_error","code":"rate_limit_per_second","retry_after_seconds":1}"#,
        );
        assert_eq!(throttled.result_status, OUTCOME_ERROR);
        assert_eq!(
            throttled.failure_kind,
            Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429)
        );
        assert!(!throttled.business_success);

        // 503 api_error -> transient vocabulary.
        let transient = keenable_facade_outcome(
            StatusCode::SERVICE_UNAVAILABLE,
            br#"{"error":"Search backend failed","type":"api_error","code":"search_upstream_error"}"#,
        );
        assert_eq!(transient.result_status, OUTCOME_ERROR);
        assert_eq!(
            transient.failure_kind,
            Some(FAILURE_KIND_UPSTREAM_GATEWAY_5XX)
        );
        assert!(!transient.business_success);
    }

    #[test]
    fn keenable_business_success_gates_on_the_signal_not_the_http_status() {
        // The keenable envelope carries type in the BODY: an HTTP 200 with an
        // authentication_error envelope is an auth failure, so a 2xx transport
        // status alone must NEVER settle a credit (todo 7 rule).
        let auth_on_200 = keenable_facade_outcome(
            StatusCode::OK,
            br#"{"error":"Authentication failed","type":"authentication_error","code":"invalid_api_key"}"#,
        );
        assert_eq!(auth_on_200.result_status, OUTCOME_ERROR);
        assert!(!auth_on_200.business_success, "a 2xx auth failure never bills");

        // Symmetrically, a non-2xx never reaches business success through the
        // default arm.
        let ok_on_400 = keenable_facade_outcome(
            StatusCode::BAD_REQUEST,
            br#"{"query":"mock","results":[]}"#,
        );
        assert_eq!(ok_on_400.result_status, OUTCOME_ERROR);
        assert!(!ok_on_400.business_success);
    }

    #[test]
    fn keenable_health_actions_split_exhaustion_auth_and_never_target_errors() {
        let exhausted = keenable_attempt_analysis(
            StatusCode::PAYMENT_REQUIRED,
            br#"{"type":"billing_error","code":"insufficient_credits"}"#,
            ProviderFailureSignal::Exhausted,
        );
        assert!(matches!(
            exhausted.key_health_action,
            KeyHealthAction::MarkExhausted
        ));
        assert_eq!(exhausted.status, OUTCOME_QUOTA_EXHAUSTED);
        assert_eq!(exhausted.tavily_status_code, Some(402));

        let auth = keenable_attempt_analysis(
            StatusCode::UNAUTHORIZED,
            br#"{"type":"authentication_error"}"#,
            ProviderFailureSignal::AuthFailed,
        );
        assert!(matches!(
            auth.key_health_action,
            KeyHealthAction::Quarantine(_)
        ));

        // target_error / target_blocked is the CALLER's problem: no key
        // action, whatever the status said.
        let target = keenable_attempt_analysis(
            StatusCode::FORBIDDEN,
            br#"{"type":"target_error","code":"target_blocked"}"#,
            ProviderFailureSignal::ClientError,
        );
        assert!(matches!(target.key_health_action, KeyHealthAction::None));
        assert_eq!(target.status, OUTCOME_ERROR);
        assert_eq!(target.tavily_status_code, Some(403));

        // Client errors, rate limits and transients never mark the key either.
        for (status, signal) in [
            (StatusCode::BAD_REQUEST, ProviderFailureSignal::ClientError),
            (
                StatusCode::TOO_MANY_REQUESTS,
                ProviderFailureSignal::RateLimited { retry_after_secs: Some(1) },
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                ProviderFailureSignal::ServerTransient,
            ),
        ] {
            let analysis = keenable_attempt_analysis(status, b"{}", signal);
            assert!(
                matches!(analysis.key_health_action, KeyHealthAction::None),
                "{status} must never mark the key"
            );
        }
    }

    #[test]
    fn keenable_passive_harvest_parser_accepts_numbers_strings_and_skips_absent() {
        let native = br#"{"usage":{"credits_remaining":99999}}"#;
        assert_eq!(parse_keenable_credits_remaining(native), Some(99_999));

        let stringified = br#"{"usage":{"credits_remaining":"99998"}}"#;
        assert_eq!(parse_keenable_credits_remaining(stringified), Some(99_998));

        // Partial / malformed responses (todo-7 adversarial): no usage object,
        // a non-numeric balance, or non-JSON bodies are simply NO sample.
        for body in [
            br#"{"query":"mock","results":[]}"#.as_slice(),
            br#"{"usage":{}}"#.as_slice(),
            br#"{"usage":{"credits_remaining":null}}"#.as_slice(),
            br#"{"usage":{"credits_remaining":"not-a-number"}}"#.as_slice(),
            br#"not json at all"#.as_slice(),
            b"".as_slice(),
        ] {
            assert_eq!(
                parse_keenable_credits_remaining(body),
                None,
                "no sample from {body:?}"
            );
        }
    }

    #[test]
    fn keenable_mcp_meta_harvest_parser_reads_single_and_batch_results() {
        // A successful tools/call carries the balance at
        // result._meta["keenable/usage"].credits_remaining (todo-8 contract).
        let single = br#"{"jsonrpc":"2.0","id":3,"result":{"content":[],"structuredContent":{},"_meta":{"keenable/usage":{"credits":1,"credits_remaining":99999}}}}"#;
        assert_eq!(parse_keenable_mcp_credits_remaining(single), Some(99_999));

        // A numeric string balance coerces like the REST parser.
        let stringified = br#"{"jsonrpc":"2.0","id":3,"result":{"_meta":{"keenable/usage":{"credits_remaining":"99998"}}}}"#;
        assert_eq!(parse_keenable_mcp_credits_remaining(stringified), Some(99_998));

        // A batch exposes it per result; the LAST observed value wins.
        let batch = br#"[{"jsonrpc":"2.0","id":1,"result":{"_meta":{"keenable/usage":{"credits_remaining":99999}}}},{"jsonrpc":"2.0","id":2,"result":{"_meta":{"keenable/usage":{"credits_remaining":99998}}}}]"#;
        assert_eq!(parse_keenable_mcp_credits_remaining(batch), Some(99_998));

        // Todo-8 adversarial: free methods (no _meta), error envelopes
        // (error at the top level), a null _meta, and non-JSON bodies are
        // simply NO sample.
        for body in [
            br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":3,"result":{"_meta":null}}"#.as_slice(),
            br#"[{"jsonrpc":"2.0","id":1,"result":{"_meta":{"keenable/usage":{}}}}]"#.as_slice(),
            br#"not json-rpc at all"#.as_slice(),
            b"".as_slice(),
        ] {
            assert_eq!(
                parse_keenable_mcp_credits_remaining(body),
                None,
                "no sample from {body:?}"
            );
        }
    }
}
