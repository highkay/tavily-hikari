//! Zhihu REST facade execution layer (keenable-integration plan todo 9).
//!
//! Layered over the generic REST seam (`proxy_provider_rest_facade`):
//! * both native GET routes (`/api/v1/content/{zhihu_search,global_search}`)
//!   are forwarded verbatim: the pooled Access Secret rides
//!   `Authorization: Bearer` and the server clock stamps a fresh
//!   `X-Request-Timestamp` (todo 4 decorator), never a client value;
//! * the response body stays Zhihu's native `{Code,Message,Data}` envelope in
//!   EVERY case - including failures, which arrive as HTTP 200 with a non-zero
//!   `Code` - so the client sees exactly the upstream bytes;
//! * the todo-3 `classify_zhihu_failure` signal drives key health: `20001`
//!   quarantines, `30001` arms the shared HTTP backoff, `90001` is transient,
//!   and `10001`/unknown codes are client errors that never mark a key;
//! * request_logs carry the provider stamp, the upstream HTTP status (always
//!   200 for the envelope) in the legacy `tavily_status_code` column, the
//!   `zhihu_http_*` operation label, and redacted bodies.
//!
//! Billing is NOT decided here: the bin boundary projects
//! [`ZhihuFacadeOutcome`] and settles 1 credit only when `business_success`
//! (the envelope's `Code == 0`) is true, so a non-zero Code can never charge
//! the wallet even though the transport status is 200.

use super::*;

/// Documented local rejections (typed 400 `invalid_request` at the bin
/// boundary, before any gate or upstream call).
const ZHIHU_QUERY_REQUIRED_MESSAGE: &str = "Query is required and must not be empty";
const ZHIHU_SEARCH_COUNT_LIMIT_MESSAGE: &str = "Count must be at most 10 for zhihu_search";
const ZHIHU_GLOBAL_SEARCH_COUNT_LIMIT_MESSAGE: &str = "Count must be at most 20 for global_search";

/// Deterministic projection of one Zhihu facade response onto the token-log
/// vocabulary plus the business-success bit the billing recorder gates on.
#[derive(Debug, Clone, Copy)]
pub struct ZhihuFacadeOutcome {
    /// `success` / `quota_exhausted` / `error` (shared OUTCOME_* vocabulary).
    pub result_status: &'static str,
    pub failure_kind: Option<&'static str>,
    /// True ONLY for the native envelope's `Code == 0`. The signal classifier
    /// falls back to the HTTP status for a body without a `Code` (a malformed
    /// 200 classifies `Ok`), so billing needs this stricter predicate.
    pub business_success: bool,
}

/// Project a buffered Zhihu upstream response without header evidence (the
/// envelope has no header-derived arm).
pub fn zhihu_facade_outcome(status: StatusCode, body: &[u8]) -> ZhihuFacadeOutcome {
    let signal = classify_provider_failure(
        providers::ProviderId::Zhihu,
        &ProviderFailureEvidence {
            status: status.as_u16(),
            headers: None,
            body,
        },
    );
    let result_status = match signal {
        ProviderFailureSignal::Ok => OUTCOME_SUCCESS,
        ProviderFailureSignal::Exhausted => OUTCOME_QUOTA_EXHAUSTED,
        ProviderFailureSignal::AuthFailed
        | ProviderFailureSignal::RateLimited { .. }
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => OUTCOME_ERROR,
    };
    ZhihuFacadeOutcome {
        result_status,
        failure_kind: signal.failure_kind(),
        business_success: zhihu_envelope_code_is_zero(body),
    }
}

/// Local Zhihu query contract (plan todo 9), enforced at the bin boundary
/// before the quota gates so a rejected call has zero upstream cost.
///
/// `Query` is required and non-empty; `Count` above the provider cap (10 for
/// `zhihu_search`, 20 for `global_search`) is a typed 400. `Count <= 0` and a
/// non-numeric `Count` pass through (the upstream default/validation owns
/// them), and `SortBy`/`Filter` are never inspected.
pub fn validate_zhihu_rest_query(
    upstream_path: &str,
    query: Option<&str>,
) -> Result<(), &'static str> {
    let mut has_query = false;
    // Repeated `Count` values take the MAXIMUM so a duplicate parameter can
    // never smuggle an over-cap request past the local guard.
    let mut count: Option<i64> = None;
    if let Some(query) = query {
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "Query" => has_query |= !value.trim().is_empty(),
                "Count" => {
                    if let Ok(parsed) = value.trim().parse::<i64>() {
                        count = Some(count.map_or(parsed, |kept| kept.max(parsed)));
                    }
                }
                _ => {}
            }
        }
    }
    if !has_query {
        return Err(ZHIHU_QUERY_REQUIRED_MESSAGE);
    }
    match upstream_path {
        "/api/v1/content/zhihu_search" if count.is_some_and(|count| count > 10) => {
            Err(ZHIHU_SEARCH_COUNT_LIMIT_MESSAGE)
        }
        "/api/v1/content/global_search" if count.is_some_and(|count| count > 20) => {
            Err(ZHIHU_GLOBAL_SEARCH_COUNT_LIMIT_MESSAGE)
        }
        _ => Ok(()),
    }
}

fn zhihu_key_health_action(
    status: StatusCode,
    body: &[u8],
    signal: ProviderFailureSignal,
) -> KeyHealthAction {
    match signal {
        // `Code=20001`: the pooled Access Secret is dead. The existing
        // quarantine machinery owns the flip; no low-quota depletion row is
        // ever written for a non-Tavily provider (M8).
        ProviderFailureSignal::AuthFailed => KeyHealthAction::Quarantine(QuarantineDecision {
            reason_code: BLOCKED_KEY_REASON_INVALID_API_KEY.to_string(),
            reason_summary: format!(
                "Zhihu rejected the pooled Access Secret as invalid (HTTP {})",
                status.as_u16()
            ),
            reason_detail: truncate_text(
                String::from_utf8_lossy(body).trim(),
                QUARANTINE_REASON_DETAIL_MAX_LEN,
            ),
        }),
        // Unreachable through the Zhihu envelope (no exhaustion code), kept
        // exhaustive for the shared signal vocabulary.
        ProviderFailureSignal::Exhausted => KeyHealthAction::MarkExhausted,
        ProviderFailureSignal::Ok
        | ProviderFailureSignal::RateLimited { .. }
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => KeyHealthAction::None,
    }
}

fn zhihu_attempt_analysis(
    status: StatusCode,
    body: &[u8],
    signal: ProviderFailureSignal,
) -> AttemptAnalysis {
    let outcome = zhihu_facade_outcome(status, body);
    AttemptAnalysis {
        status: outcome.result_status,
        // The legacy column carries the upstream HTTP status verbatim (200
        // for every native envelope, success or Code error).
        tavily_status_code: Some(i64::from(status.as_u16())),
        key_health_action: zhihu_key_health_action(status, body, signal),
        failure_kind: outcome.failure_kind.map(str::to_string),
        key_effect: KeyEffect::none(),
        api_key_id: None,
    }
}

impl TavilyProxy {
    /// Post-response bookkeeping for the Zhihu REST facades (mirror of the
    /// Exa record path): envelope classification via the todo-3 signal,
    /// reconcile (20001 quarantine), 30001 shared HTTP backoff arming,
    /// transient-backoff clearing on `Code=0`, and the provider-stamped
    /// request_logs row with redacted bodies.
    pub(crate) async fn record_zhihu_rest_facade_attempt(
        &self,
        ctx: &ProviderFacadeAttemptContext<'_>,
    ) -> Result<ProviderFacadeAttemptRecord, ProxyError> {
        if ctx.request.provider != providers::ProviderId::Zhihu {
            return Ok(ProviderFacadeAttemptRecord::boundary_default());
        }

        let analysis = zhihu_attempt_analysis(ctx.status, ctx.body_bytes, ctx.signal);
        let mut key_effect = self
            .reconcile_key_health(
                ctx.lease,
                ctx.request.path.as_str(),
                &analysis,
                ctx.request.auth_token_id.as_deref(),
            )
            .await?;
        if key_effect.code == KEY_EFFECT_NONE && analysis.status == OUTCOME_SUCCESS {
            key_effect = self
                .clear_transient_backoffs_after_success(
                    &ctx.lease.id,
                    ctx.request.path.as_str(),
                    ctx.request.auth_token_id.as_deref(),
                )
                .await?;
        }

        // `Code=30001` is throughput, never exhaustion: arm the shared HTTP
        // backoff scope that provider-scoped acquisition avoids. The code
        // carries no Retry-After, so the todo-11 second-scale lane (60s
        // default, 5..=300 clamp) applies.
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

        // Secrets redacted before persistence (M17): the pooled Bearer value
        // and any client th- token can never survive into the stored query or
        // bodies.
        let redacted_request_body = redact_api_key_bytes(&ctx.request.body);
        let redacted_response_body = redact_api_key_bytes(ctx.body_bytes);
        let request_log_id = self
            .key_store
            .log_attempt(AttemptLog {
                key_id: Some(&ctx.lease.id),
                auth_token_id: ctx.request.auth_token_id.as_deref(),
                method: &ctx.request.method,
                path: ctx.request.path.as_str(),
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

    /// Transport-failure audit row for the Zhihu REST facades (the closed
    /// envelope never produces this arm; a dead connection still keeps the
    /// provider-scoped trail).
    pub(crate) async fn log_zhihu_rest_facade_transport_error(
        &self,
        request: &ProviderRestFacadeRequest,
        lease: &ApiKeyLease,
        sanitized_headers: &SanitizedHeaders,
        err: &ProxyError,
        fallback_reason: Option<&'static str>,
    ) -> Result<(), ProxyError> {
        if request.provider != providers::ProviderId::Zhihu {
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
                path: request.path.as_str(),
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
mod zhihu_query_guard_tests {
    use super::*;

    #[test]
    fn query_must_be_present_and_non_blank() {
        for query in [None, Some(""), Some("Count=3"), Some("Query="), Some("Query=%20%20")] {
            assert_eq!(
                validate_zhihu_rest_query("/api/v1/content/zhihu_search", query),
                Err(ZHIHU_QUERY_REQUIRED_MESSAGE),
                "{query:?}"
            );
        }
        for query in [
            "Query=AI",
            "Query=%E4%B8%AD%E6%96%87&Count=3",
            "Count=0&Query=AI&SortBy=up_vote",
        ] {
            assert_eq!(
                validate_zhihu_rest_query("/api/v1/content/zhihu_search", Some(query)),
                Ok(()),
                "{query}"
            );
        }
    }

    #[test]
    fn count_caps_are_per_endpoint_and_non_positive_values_pass_through() {
        for count in [11i64, 50] {
            assert_eq!(
                validate_zhihu_rest_query(
                    "/api/v1/content/zhihu_search",
                    Some(&format!("Query=AI&Count={count}"))
                ),
                Err(ZHIHU_SEARCH_COUNT_LIMIT_MESSAGE),
                "{count}"
            );
        }
        for count in [21i64, 50] {
            assert_eq!(
                validate_zhihu_rest_query(
                    "/api/v1/content/global_search",
                    Some(&format!("Query=AI&Count={count}"))
                ),
                Err(ZHIHU_GLOBAL_SEARCH_COUNT_LIMIT_MESSAGE),
                "{count}"
            );
        }
        assert_eq!(
            validate_zhihu_rest_query("/api/v1/content/global_search", Some("Query=AI&Count=20")),
            Ok(())
        );
        assert_eq!(
            validate_zhihu_rest_query("/api/v1/content/zhihu_search", Some("Query=AI&Count=10")),
            Ok(())
        );
        for count in [0i64, -1] {
            assert_eq!(
                validate_zhihu_rest_query(
                    "/api/v1/content/zhihu_search",
                    Some(&format!("Query=AI&Count={count}"))
                ),
                Ok(()),
                "{count}"
            );
        }
        // The upstream owns non-numeric Count validation.
        assert_eq!(
            validate_zhihu_rest_query("/api/v1/content/zhihu_search", Some("Query=AI&Count=lots")),
            Ok(())
        );
    }

    #[test]
    fn business_success_requires_an_exact_zero_code() {
        assert!(zhihu_envelope_code_is_zero(br#"{"Code":0,"Message":"ok"}"#));
        assert!(zhihu_envelope_code_is_zero(br#"{"Code":"0"}"#));
        for body in [
            br#"{"Code":10001}"#.as_slice(),
            br#"{"Code":20001}"#.as_slice(),
            br#"{"Code":0.5}"#.as_slice(),
            br#"not json"#.as_slice(),
            br#"{"Message":"no code"}"#.as_slice(),
        ] {
            assert!(!zhihu_envelope_code_is_zero(body), "{body:?}");
        }
    }

    #[test]
    fn malformed_or_success_shaped_error_envelopes_never_bill() {
        // Adversarial: a 200 body without `Code` classifies `Ok` via the
        // status fallback, so the signal alone would bill; the projection's
        // exact-Code predicate keeps it zero-credit. A non-zero Code that
        // carries success-shaped Data must bill nothing either.
        let malformed = zhihu_facade_outcome(StatusCode::OK, b"not an envelope");
        assert_eq!(malformed.result_status, OUTCOME_SUCCESS);
        assert!(!malformed.business_success);

        let misleading = zhihu_facade_outcome(
            StatusCode::OK,
            br#"{"Code":20001,"Message":"Authorization failed","Data":{"Items":[{"ContentType":"Article"}]}}"#,
        );
        assert_eq!(misleading.result_status, OUTCOME_ERROR);
        assert_eq!(
            misleading.failure_kind,
            Some(FAILURE_KIND_UPSTREAM_ACCOUNT_DEACTIVATED_401)
        );
        assert!(!misleading.business_success);

        let happy = zhihu_facade_outcome(
            StatusCode::OK,
            br#"{"Code":0,"Message":"success","Data":{}}"#,
        );
        assert_eq!(happy.result_status, OUTCOME_SUCCESS);
        assert!(happy.business_success);
    }
}
