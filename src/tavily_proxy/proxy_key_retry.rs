// Bounded cross-key retry policy kernel (plan provider-key-429-retry todo 4).
//
// ZERO-I/O decision surface: no store reads, no clock reads, no network. The
// caller injects every observation - classification signal, effective status,
// attempt evidence, the already-parsed Retry-After, attempts used, elapsed time
// and whether the todo-5 strict acquire found a candidate - so the whole
// policy stays a pure function and its table is unit-testable without
// fixtures, a database or a wall clock.
//
// Contract - the 2026-10-16 user amendment supersedes the frozen
// "explicit rejection only" whitelist (plan banner + todo 4 AMENDMENT note):
// * retryable, arm 1 (explicit rejection) - a *provably not executed*
//   rejection stays replayable on another key: 429 rate limit, 432/402
//   exhaustion and the 401/403 auth-block family;
// * retryable, arm 2 (key-scoped transient) - an upstream 5xx
//   (`ServerTransient`) and transport send-error / timeout evidence now judge
//   `Retry` as well: none of them is caused by the request, so one bounded try
//   on another key beats surfacing the failure to the client;
// * vetoed - Exa 429 and Exa team-scoped 402, because they are team-level
//   limits another key cannot lift (unchanged by the amendment);
// * never retryable - a request-shaped 4xx (400/404/406/422/...): the request
//   itself is wrong, so rotating a key cannot fix it and marking the key would
//   only burn the pool. The caller must neither mark nor rotate on
//   `RequestShapedClientError`;
// * exclusions - `/research` (POST *and* GET) and `/crawl` are never replayed
//   under either arm, and a Retry-After beyond the fast-path window is served
//   as-is: the kernel never sleeps, so a long upstream wait is never absorbed
//   silently;
// * budget - at most `KEY_RETRY_MAX_ATTEMPTS_DEFAULT` upstream attempts per
//   logical request (ONE retry) inside a 5s deadline. Both are env-tunable
//   from the single override point below; v1 deliberately bypasses
//   SystemSettings and the admin UI (a UI switch is a separate spec);
// * no new effect codes and no dispatch/reservation/settlement writes: billing
//   keeps settling once on the final response and the caller marks the retry
//   chain through `fallback_reason`.
//
// Consumers: the todo-6 Tavily HTTP executor (`proxy_http_and_logs.rs`) calls
// `decide_cross_key_retry` / `KeyRetryBudget::from_env` in production, so the
// staged-seam `allow`s are gone from everything that path consumes. Still
// staged (allow kept, named at each item): `is_retry_eligible` /
// `key_retry_ineligibility` (the default-window predicate the table tests pin),
// `KeyRetryAttemptEvidence::exa_budget` and
// `KeyRetryRequestKind::ProviderRestRead` - they land with todo 9 (provider
// REST facades), which fills the Exa budget-tag evidence todo 10 moved into
// the classifier (`crate::analysis::exa_budget_tag_evidence`).

/// Attempt ceiling default: the first upstream attempt plus ONE cross-key
/// retry. The plan freezes "at most one retry", so this is also the ceiling
/// the env override is clamped to.
pub(crate) const KEY_RETRY_MAX_ATTEMPTS_DEFAULT: u32 = 2;

/// Wall-clock budget default for the whole logical request. The kernel does
/// not read a clock; the caller passes its own elapsed time.
pub(crate) const KEY_RETRY_DEADLINE_SECS_DEFAULT: u64 = 5;

/// A rejection is only replayed immediately when the armed Retry-After fits
/// inside this window; anything longer is served as-is. The gate spans both
/// retry arms, because the kernel never sleeps.
pub(crate) const KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_DEFAULT: u64 = 2;

/// Structural ceiling for the attempt budget: env may lower it to `1` (retry
/// off) but never raise it past one retry, which is what keeps the
/// single-billing and no-replay arguments local.
pub(crate) const KEY_RETRY_MAX_ATTEMPTS_CEILING: u32 = 2;

/// Sanity bound for both second budgets; a typo falls back to the default
/// with a startup-style warning instead of silently disabling the deadline.
pub(crate) const KEY_RETRY_SECS_CEILING: u64 = 60;

pub(crate) const KEY_RETRY_MAX_ATTEMPTS_ENV: &str = "KEY_RETRY_MAX_ATTEMPTS";
pub(crate) const KEY_RETRY_DEADLINE_SECS_ENV: &str = "KEY_RETRY_DEADLINE_SECS";
pub(crate) const KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_ENV: &str =
    "KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS";

/// Request surface a retry decision is asked about.
///
/// The kernel never inspects the request itself; the caller maps its endpoint
/// onto one variant. `Research` covers BOTH the POST submission handler and
/// the GET result poll. MCP tool surfaces are intentionally absent: the todo-13
/// MCP ladder owns session rebind and never enters this kernel in v1 (Exa and
/// Brave local MCP stay single-attempt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRetryRequestKind {
    /// Tavily HTTP read endpoints (search/extract/map/usage): replay-safe.
    HttpRead,
    /// Provider REST facade read endpoints (serpapi/exa/brave search).
    /// Staged for plan todo 9: the todo-6 Tavily HTTP seam never maps a path
    /// onto this surface, so no production code constructs it yet.
    #[allow(dead_code)]
    ProviderRestRead,
    /// `/research` POST + GET: a research task is never replayed.
    Research,
    /// `/crawl`: never replayed.
    Crawl,
}

impl KeyRetryRequestKind {
    pub(crate) fn allows_cross_key_retry(self) -> bool {
        matches!(self, Self::HttpRead | Self::ProviderRestRead)
    }
}

// The Exa 402 budget-tag scope lives in the CLASSIFIER
// (`crate::analysis::ExaBudgetTag`, in scope here through the module-level
// `use crate::analysis::*`), because that is where the upstream `tag` literals
// are parsed - plan todo 10 made it the single source of truth. The kernel
// only consumes the already-parsed scope as
// `KeyRetryAttemptEvidence::exa_budget_tag`, which is what keeps this file
// zero-I/O and the literals down to one definition.

/// Transport-layer failure detail for an attempt that never produced an HTTP
/// response. The caller derives it from its own `ProxyError` / `reqwest::Error`
/// (`is_timeout()`, `is_connect()`, anything else on the send path - the
/// `FAILURE_KIND_TRANSPORT_SEND_ERROR` territory); the kernel never touches the
/// error object, which is what keeps it zero-I/O. The variants mirror
/// `TransportFailureKind` so a caller maps them one-to-one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRetryTransportFailure {
    /// A deadline expired before the upstream answered.
    Timeout,
    /// TCP/TLS connect failure.
    Connect,
    /// Any other send-side failure: DNS, connection reset, broken pipe.
    Send,
}

/// Caller-injected attempt evidence the eligibility predicate needs beyond the
/// normalized signal. Not body-only: a transport failure has neither a status
/// nor a body, so its detail rides here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyRetryAttemptEvidence {
    /// Exa 402 budget-tag scope when the body carried one. Filled from the
    /// classifier's `exa_budget_tag_evidence(body)` (plan todo 10), which owns
    /// the tag literals; `None` keeps an Exa 402 vetoed.
    pub(crate) exa_budget_tag: Option<ExaBudgetTag>,
    /// Set when the attempt died on the transport layer (send error/timeout).
    pub(crate) transport_failure: Option<KeyRetryTransportFailure>,
}

impl KeyRetryAttemptEvidence {
    pub(crate) const fn none() -> Self {
        Self {
            exa_budget_tag: None,
            transport_failure: None,
        }
    }

    /// Staged for plan todo 9: the Exa facade builds this from the todo-10
    /// classifier evidence.
    #[allow(dead_code)]
    pub(crate) const fn exa_budget(tag: ExaBudgetTag) -> Self {
        Self {
            exa_budget_tag: Some(tag),
            transport_failure: None,
        }
    }

    pub(crate) const fn transport(kind: KeyRetryTransportFailure) -> Self {
        Self {
            exa_budget_tag: None,
            transport_failure: Some(kind),
        }
    }
}

/// The three explicit-rejection families the retry whitelist allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRetryRejectionFamily {
    RateLimit,
    Exhaustion,
    AuthBlock,
}

/// Why an attempt may be replayed on another key.
///
/// The 2026-10-16 amendment added the second arm: an upstream 5xx or a
/// transport send/timeout failure is not a *provably unexecuted* rejection, but
/// it is not caused by the request either, so one bounded try on another key is
/// worth more than surfacing it. Replay safety comes from the request-surface
/// exclusion (`/research`, `/crawl`), never from the trigger class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRetryTrigger {
    /// Provably-unexecuted explicit rejection: 429 / 432 / 402 / 401-403.
    ExplicitRejection(KeyRetryRejectionFamily),
    /// Key-scoped transient failure: upstream 5xx or transport send/timeout.
    Transient,
}

/// Resolved retry budget. Build it through [`KeyRetryBudget::from_env`] (the
/// single override point) or [`KeyRetryBudget::defaults`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyRetryBudget {
    pub(crate) max_attempts: u32,
    pub(crate) deadline: std::time::Duration,
    pub(crate) rate_limit_fast_path: std::time::Duration,
}

impl KeyRetryBudget {
    /// Documented defaults with no env read: 2 attempts (1 retry), 5s deadline,
    /// 2s rate-limit fast path.
    pub(crate) const fn defaults() -> Self {
        Self {
            max_attempts: KEY_RETRY_MAX_ATTEMPTS_DEFAULT,
            deadline: std::time::Duration::from_secs(KEY_RETRY_DEADLINE_SECS_DEFAULT),
            rate_limit_fast_path: std::time::Duration::from_secs(
                KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_DEFAULT,
            ),
        }
    }

    /// THE single env override point for the retry budget. v1 does not touch
    /// SystemSettings or the admin UI, so operators tune it through
    /// `KEY_RETRY_MAX_ATTEMPTS`, `KEY_RETRY_DEADLINE_SECS` and
    /// `KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS`.
    pub(crate) fn from_env() -> Self {
        parse_key_retry_budget(
            std::env::var(KEY_RETRY_MAX_ATTEMPTS_ENV).ok().as_deref(),
            std::env::var(KEY_RETRY_DEADLINE_SECS_ENV).ok().as_deref(),
            std::env::var(KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_ENV)
                .ok()
                .as_deref(),
        )
    }
}

/// Pure budget parser behind [`KeyRetryBudget::from_env`]: empty or missing
/// values keep the default, anything unparsable or out of range warns and
/// keeps the default (mirrors `parse_low_quota_depletion_threshold`).
pub(crate) fn parse_key_retry_budget(
    max_attempts_raw: Option<&str>,
    deadline_secs_raw: Option<&str>,
    rate_limit_fast_path_secs_raw: Option<&str>,
) -> KeyRetryBudget {
    let defaults = KeyRetryBudget::defaults();
    let max_attempts = parse_key_retry_bounded(
        max_attempts_raw,
        KEY_RETRY_MAX_ATTEMPTS_ENV,
        1..=u64::from(KEY_RETRY_MAX_ATTEMPTS_CEILING),
        u64::from(defaults.max_attempts),
    ) as u32;
    let deadline_secs = parse_key_retry_bounded(
        deadline_secs_raw,
        KEY_RETRY_DEADLINE_SECS_ENV,
        0..=KEY_RETRY_SECS_CEILING,
        KEY_RETRY_DEADLINE_SECS_DEFAULT,
    );
    let fast_path_secs = parse_key_retry_bounded(
        rate_limit_fast_path_secs_raw,
        KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_ENV,
        0..=KEY_RETRY_SECS_CEILING,
        KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_DEFAULT,
    );
    KeyRetryBudget {
        max_attempts,
        deadline: std::time::Duration::from_secs(deadline_secs),
        rate_limit_fast_path: std::time::Duration::from_secs(fast_path_secs),
    }
}

fn parse_key_retry_bounded(
    raw: Option<&str>,
    source: &str,
    range: std::ops::RangeInclusive<u64>,
    default: u64,
) -> u64 {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return default;
    };
    match raw.parse::<u64>() {
        Ok(value) if range.contains(&value) => value,
        _ => {
            eprintln!(
                "warning: {source} must be an integer in {}..={}; using default {default}",
                range.start(),
                range.end()
            );
            default
        }
    }
}

/// Why a rejection is NOT replayable. `AttemptsExhausted` lives here because
/// the signal itself stays non-retryable once the attempt budget is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyRetryIneligibleReason {
    /// Success, a redirect, or an attempt carrying neither a retryable status
    /// nor transport evidence: nothing to replay on another key.
    NotRetryableOutcome,
    /// Request-shaped client error (400/404/406/422/...): the request itself is
    /// wrong, so another key cannot fix it. The caller must neither mark nor
    /// rotate the key on this reason - doing so would only burn the pool.
    RequestShapedClientError,
    /// `/research` (POST + GET) or `/crawl`.
    RequestKindExcluded,
    /// Exa 429 or Exa team-scoped 402: a team-level limit another key cannot
    /// lift (conservative default, recorded as an assumption by todo 17).
    ProviderVeto,
    /// The armed Retry-After is beyond the fast-path window and the kernel
    /// never sleeps, so the first response is served unchanged.
    RetryAfterBeyondFastPath,
    /// The attempt budget is spent: at most ONE cross-key retry per request.
    AttemptsExhausted,
}

/// Why the buffered first-hit response goes back to the client byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServeOriginalReason {
    Ineligible(KeyRetryIneligibleReason),
    DeadlineExceeded,
    NoEligibleKey,
}

/// Kernel verdict. `ServeOriginal` never reuses the 502 path: the caller
/// returns the buffered first-hit response (status, Retry-After header, body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    Retry,
    ServeOriginal(ServeOriginalReason),
}

/// Caller-injected observations. `attempts_used` counts upstream attempts
/// already completed for this logical request (1 right after the first hit);
/// `elapsed` comes from the caller's own deadline (`BackendTime` / `Instant`),
/// never from a clock read inside the kernel; `eligible_key_available` is the
/// todo-5 strict acquire probe (`Ok(None)` => false). A caller that acquires
/// only after a positive verdict passes `true` here and maps the empty-pool
/// outcome onto `ServeOriginal(ServeOriginalReason::NoEligibleKey)` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyRetryContext {
    pub(crate) attempts_used: u32,
    pub(crate) elapsed: std::time::Duration,
    pub(crate) eligible_key_available: bool,
}

/// Whitelist predicate named by the plan: true only for a key-scoped failure -
/// an explicit rejection, or a transient 5xx / transport failure - on a
/// replayable request surface, with no provider veto and a Retry-After inside
/// the default fast-path window. Production callers should prefer
/// [`decide_cross_key_retry`], which applies the *configured* budget instead of
/// the defaults.
#[allow(dead_code)]
pub(crate) fn is_retry_eligible(
    provider: providers::ProviderId,
    request_kind: KeyRetryRequestKind,
    signal: ProviderFailureSignal,
    effective_status: Option<u16>,
    evidence: &KeyRetryAttemptEvidence,
    retry_after_secs: Option<u64>,
) -> bool {
    key_retry_ineligibility(
        provider,
        request_kind,
        signal,
        effective_status,
        evidence,
        retry_after_secs,
    )
    .is_none()
}

/// Reason detail behind [`is_retry_eligible`], evaluated against the default
/// fast-path window.
#[allow(dead_code)]
pub(crate) fn key_retry_ineligibility(
    provider: providers::ProviderId,
    request_kind: KeyRetryRequestKind,
    signal: ProviderFailureSignal,
    effective_status: Option<u16>,
    evidence: &KeyRetryAttemptEvidence,
    retry_after_secs: Option<u64>,
) -> Option<KeyRetryIneligibleReason> {
    key_retry_ineligibility_within(
        provider,
        request_kind,
        signal,
        effective_status,
        evidence,
        retry_after_secs,
        KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_DEFAULT,
    )
}

fn key_retry_ineligibility_within(
    provider: providers::ProviderId,
    request_kind: KeyRetryRequestKind,
    signal: ProviderFailureSignal,
    effective_status: Option<u16>,
    evidence: &KeyRetryAttemptEvidence,
    retry_after_secs: Option<u64>,
    rate_limit_fast_path_secs: u64,
) -> Option<KeyRetryIneligibleReason> {
    // Checked first on purpose: the surface boundary is the durable rule an
    // operator needs to see, and it beats eligibility - a 5xx or a transport
    // timeout on `/research` reports the exclusion, never the transient arm.
    if !request_kind.allows_cross_key_retry() {
        return Some(KeyRetryIneligibleReason::RequestKindExcluded);
    }
    let Some(trigger) = key_retry_trigger(signal, effective_status, evidence) else {
        return Some(if is_request_shaped_client_status(effective_status) {
            KeyRetryIneligibleReason::RequestShapedClientError
        } else {
            KeyRetryIneligibleReason::NotRetryableOutcome
        });
    };
    if exa_cross_key_retry_vetoed(provider, trigger, evidence) {
        return Some(KeyRetryIneligibleReason::ProviderVeto);
    }
    // The Retry-After gate spans BOTH arms: the kernel never sleeps, so a long
    // upstream wait is never absorbed silently even when the trigger is a 5xx
    // whose response happened to carry a Retry-After.
    if effective_retry_after_secs(signal, retry_after_secs)
        .is_some_and(|secs| secs > rate_limit_fast_path_secs)
    {
        return Some(KeyRetryIneligibleReason::RetryAfterBeyondFastPath);
    }
    None
}

/// Retry trigger of an attempt, or `None` when the attempt is neither an
/// explicit rejection nor a key-scoped transient failure.
///
/// Precedence, highest first:
/// 1. an explicit rejection on the normalized signal - key-scoped whatever the
///    numeric status says (Brave reports an invalid subscription token as 422);
/// 2. a request-shaped 4xx - it proves the upstream answered and blamed the
///    request, so it outranks every other reading of the same attempt;
/// 3. transport evidence, or a `ServerTransient` signal - the transient arm,
///    which also covers a timeout a caller reports with no status at all;
/// 4. the effective status, which only *promotes* a signal carrying no
///    rejection of its own. That is what makes an HTTP 200 whose body says
///    `status: 432` eligible (the caller passes `tavily_status.or(http_status)`)
///    and a bare 503 retryable without a signal.
pub(crate) fn key_retry_trigger(
    signal: ProviderFailureSignal,
    effective_status: Option<u16>,
    evidence: &KeyRetryAttemptEvidence,
) -> Option<KeyRetryTrigger> {
    use KeyRetryRejectionFamily as Family;
    use KeyRetryTrigger as Trigger;

    let rejection = match signal {
        ProviderFailureSignal::RateLimited { .. } => Some(Family::RateLimit),
        ProviderFailureSignal::Exhausted => Some(Family::Exhaustion),
        ProviderFailureSignal::AuthFailed => Some(Family::AuthBlock),
        ProviderFailureSignal::Ok
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => None,
    };
    if let Some(family) = rejection {
        return Some(Trigger::ExplicitRejection(family));
    }
    if is_request_shaped_client_status(effective_status) {
        return None;
    }
    if evidence.transport_failure.is_some() || signal == ProviderFailureSignal::ServerTransient {
        return Some(Trigger::Transient);
    }
    effective_status.and_then(key_retry_trigger_from_status)
}

/// A 4xx the whitelist does not claim is request-shaped: 401/402/403/429/432
/// are key-scoped and handled by the family table below, every other 4xx blames
/// the request itself.
fn is_request_shaped_client_status(effective_status: Option<u16>) -> bool {
    matches!(
        effective_status,
        Some(status @ 400..=499) if !matches!(status, 401 | 402 | 403 | 429 | 432)
    )
}

fn key_retry_trigger_from_status(effective_status: u16) -> Option<KeyRetryTrigger> {
    use KeyRetryRejectionFamily as Family;
    use KeyRetryTrigger as Trigger;

    match effective_status {
        429 => Some(Trigger::ExplicitRejection(Family::RateLimit)),
        432 | 402 => Some(Trigger::ExplicitRejection(Family::Exhaustion)),
        401 | 403 => Some(Trigger::ExplicitRejection(Family::AuthBlock)),
        500..=599 => Some(Trigger::Transient),
        _ => None,
    }
}

fn exa_cross_key_retry_vetoed(
    provider: providers::ProviderId,
    trigger: KeyRetryTrigger,
    evidence: &KeyRetryAttemptEvidence,
) -> bool {
    if provider != providers::ProviderId::Exa {
        return false;
    }
    match trigger {
        // The Exa vetoes are scoped to its key-vs-team limits: a 5xx or a
        // transport failure is not a team budget, so another key may still help.
        KeyRetryTrigger::Transient => false,
        KeyRetryTrigger::ExplicitRejection(family) => match family {
            // Exa 429 is treated as team-scoped until upstream documentation
            // says otherwise, so another key cannot help.
            KeyRetryRejectionFamily::RateLimit => true,
            // Only an explicitly KEY-scoped 402 tag is replayable; a team tag or
            // missing evidence stays vetoed (conservative default).
            KeyRetryRejectionFamily::Exhaustion => !evidence
                .exa_budget_tag
                .is_some_and(ExaBudgetTag::is_key_scoped),
            KeyRetryRejectionFamily::AuthBlock => false,
        },
    }
}

/// Conservative union of the armed Retry-After and the classifier's header
/// parse: when they disagree the longer wait wins, so a long upstream wait is
/// never replayed as an immediate retry.
fn effective_retry_after_secs(signal: ProviderFailureSignal, armed: Option<u64>) -> Option<u64> {
    let from_signal = match signal {
        ProviderFailureSignal::RateLimited { retry_after_secs } => retry_after_secs,
        _ => None,
    };
    match (armed, from_signal) {
        (Some(armed), Some(signalled)) => Some(armed.max(signalled)),
        (armed, signalled) => armed.or(signalled),
    }
}

/// Authoritative verdict: eligibility (with the *configured* fast-path window),
/// then attempt budget, then deadline, then pool availability.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_cross_key_retry(
    provider: providers::ProviderId,
    request_kind: KeyRetryRequestKind,
    signal: ProviderFailureSignal,
    effective_status: Option<u16>,
    evidence: &KeyRetryAttemptEvidence,
    retry_after_secs: Option<u64>,
    context: &KeyRetryContext,
    budget: &KeyRetryBudget,
) -> RetryDecision {
    if let Some(reason) = key_retry_ineligibility_within(
        provider,
        request_kind,
        signal,
        effective_status,
        evidence,
        retry_after_secs,
        budget.rate_limit_fast_path.as_secs(),
    ) {
        return RetryDecision::ServeOriginal(ServeOriginalReason::Ineligible(reason));
    }
    if context.attempts_used >= budget.max_attempts {
        return RetryDecision::ServeOriginal(ServeOriginalReason::Ineligible(
            KeyRetryIneligibleReason::AttemptsExhausted,
        ));
    }
    if context.elapsed >= budget.deadline {
        return RetryDecision::ServeOriginal(ServeOriginalReason::DeadlineExceeded);
    }
    if !context.eligible_key_available {
        return RetryDecision::ServeOriginal(ServeOriginalReason::NoEligibleKey);
    }
    RetryDecision::Retry
}

#[cfg(test)]
mod proxy_key_retry_tests {
    use super::*;
    use crate::providers::ProviderId;
    use std::time::Duration;

    /// One table row: provider x request surface x signal x effective status x
    /// attempt evidence x Retry-After -> verdict.
    struct EligibilityCase {
        name: &'static str,
        provider: ProviderId,
        request_kind: KeyRetryRequestKind,
        signal: ProviderFailureSignal,
        effective_status: Option<u16>,
        evidence: KeyRetryAttemptEvidence,
        armed_retry_after_secs: Option<u64>,
        expected: Option<KeyRetryIneligibleReason>,
    }

    const NO_EVIDENCE: KeyRetryAttemptEvidence = KeyRetryAttemptEvidence::none();

    fn eligibility_cases() -> Vec<EligibilityCase> {
        use KeyRetryIneligibleReason as Reason;
        use KeyRetryRequestKind as Kind;
        use ProviderFailureSignal as Signal;

        vec![
            // --- whitelist: rate limit -------------------------------------
            EligibilityCase {
                name: "tavily_429_no_retry_after_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::RateLimited {
                    retry_after_secs: None,
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "tavily_429_zero_retry_after_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(0),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(0),
                expected: None,
            },
            EligibilityCase {
                name: "tavily_429_retry_after_at_fast_path_boundary_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(2),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(2),
                expected: None,
            },
            EligibilityCase {
                name: "tavily_429_retry_after_beyond_fast_path_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(3),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(3),
                expected: Some(Reason::RetryAfterBeyondFastPath),
            },
            EligibilityCase {
                name: "tavily_429_long_signalled_retry_after_wins_over_zero_armed",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(30),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(0),
                expected: Some(Reason::RetryAfterBeyondFastPath),
            },
            // --- whitelist: exhaustion -------------------------------------
            EligibilityCase {
                name: "tavily_432_exhausted_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::Exhausted,
                effective_status: Some(432),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // HTTP 200 whose body carries `status: 432`: the caller passes
                // tavily_status.or(http_status), so the effective code promotes
                // an `Ok` signal into the exhaustion family.
                name: "tavily_http_200_body_status_432_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::Ok,
                effective_status: Some(432),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "serpapi_429_run_out_of_searches_exhausted_retries",
                provider: ProviderId::Serpapi,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "brave_402_exhausted_retries",
                provider: ProviderId::Brave,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(402),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            // --- whitelist: auth block --------------------------------------
            EligibilityCase {
                name: "tavily_bare_403_auth_block_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::AuthFailed,
                effective_status: Some(403),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "tavily_bare_401_auth_block_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::AuthFailed,
                effective_status: Some(401),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // A Brave invalid subscription token arrives as 422 with a body
                // code: the signal is AuthFailed, so the key is bad even though
                // the status is outside the numeric whitelist.
                name: "brave_invalid_subscription_token_auth_block_retries",
                provider: ProviderId::Brave,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::AuthFailed,
                effective_status: Some(422),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // Exa 403 classifies as ClientError; the effective code 403
                // promotes it into the auth-block family.
                name: "exa_403_promoted_to_auth_block_retries",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ClientError,
                effective_status: Some(403),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            // --- Exa vetoes --------------------------------------------------
            EligibilityCase {
                name: "exa_429_is_vetoed",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(0),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(0),
                expected: Some(Reason::ProviderVeto),
            },
            EligibilityCase {
                name: "exa_402_team_budget_is_vetoed",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(402),
                evidence: KeyRetryAttemptEvidence::exa_budget(ExaBudgetTag::TeamBudgetExceeded),
                armed_retry_after_secs: None,
                expected: Some(Reason::ProviderVeto),
            },
            EligibilityCase {
                // No tag evidence yet (todo 10 populates it): stay conservative.
                name: "exa_402_without_tag_evidence_is_vetoed",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(402),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::ProviderVeto),
            },
            EligibilityCase {
                name: "exa_402_api_key_budget_retries",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(402),
                evidence: KeyRetryAttemptEvidence::exa_budget(ExaBudgetTag::ApiKeyBudgetExceeded),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "exa_402_no_more_credits_retries",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::Exhausted,
                effective_status: Some(402),
                evidence: KeyRetryAttemptEvidence::exa_budget(ExaBudgetTag::NoMoreCredits),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "exa_401_auth_block_retries",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::AuthFailed,
                effective_status: Some(401),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            // --- amendment arm 2: key-scoped transient -------------------------
            EligibilityCase {
                name: "tavily_500_server_transient_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ServerTransient,
                effective_status: Some(500),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "serpapi_502_gateway_retries",
                provider: ProviderId::Serpapi,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ServerTransient,
                effective_status: Some(502),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // Defensive promotion pin: a 5xx stays retryable even when the
                // classifier named a non-transient signal (Brave's body-code path
                // can return `ClientError` at any status), so the transient arm
                // never depends on the signal alone.
                name: "brave_503_with_client_error_signal_still_promotes_to_transient",
                provider: ProviderId::Brave,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ClientError,
                effective_status: Some(503),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // The Exa vetoes are scoped to its key-vs-team limits, so a 5xx on
                // Exa is still worth one bounded try on another key.
                name: "exa_503_server_transient_is_not_vetoed_retries",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ServerTransient,
                effective_status: Some(503),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "exa_402_unknown_tag_promotes_to_exhaustion_and_is_vetoed",
                provider: ProviderId::Exa,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ClientError,
                effective_status: Some(402),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::ProviderVeto),
            },
            EligibilityCase {
                // A caller that reports a timeout through the signal alone: no
                // response arrived, so there is no status to classify.
                name: "timeout_as_server_transient_without_status_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ServerTransient,
                effective_status: None,
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "transport_timeout_evidence_retries",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ServerTransient,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Timeout),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "transport_send_error_evidence_retries",
                provider: ProviderId::Serpapi,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ServerTransient,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Send),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                name: "transport_connect_error_evidence_retries",
                provider: ProviderId::Brave,
                request_kind: Kind::ProviderRestRead,
                signal: Signal::ServerTransient,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Connect),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // Transport evidence is sufficient on its own: the signal a caller
                // can still name for a dead send carries no information.
                name: "transport_evidence_retries_even_with_a_neutral_signal",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::Ok,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Send),
                armed_retry_after_secs: None,
                expected: None,
            },
            EligibilityCase {
                // The Retry-After gate spans both arms: the kernel never sleeps, so
                // a 5xx carrying a long Retry-After is served unchanged.
                name: "tavily_500_with_long_retry_after_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ServerTransient,
                effective_status: Some(500),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(30),
                expected: Some(Reason::RetryAfterBeyondFastPath),
            },
            // --- amendment boundary: request-shaped 4xx never rotates a key -------
            EligibilityCase {
                name: "tavily_400_client_error_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ClientError,
                effective_status: Some(400),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestShapedClientError),
            },
            EligibilityCase {
                name: "tavily_404_not_found_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ClientError,
                effective_status: Some(404),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestShapedClientError),
            },
            EligibilityCase {
                name: "tavily_406_not_acceptable_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ClientError,
                effective_status: Some(406),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestShapedClientError),
            },
            EligibilityCase {
                name: "tavily_422_validation_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ClientError,
                effective_status: Some(422),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestShapedClientError),
            },
            EligibilityCase {
                // A concrete 4xx outranks contradictory transport evidence: the
                // status proves the upstream answered and blamed the request.
                name: "request_shaped_400_outranks_transport_evidence",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::ServerTransient,
                effective_status: Some(400),
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Timeout),
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestShapedClientError),
            },
            EligibilityCase {
                name: "success_serves_original",
                provider: ProviderId::Tavily,
                request_kind: Kind::HttpRead,
                signal: Signal::Ok,
                effective_status: Some(200),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::NotRetryableOutcome),
            },
            // --- request-surface exclusions -----------------------------------
            EligibilityCase {
                name: "research_post_429_is_excluded",
                provider: ProviderId::Tavily,
                request_kind: Kind::Research,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(0),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(0),
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                // The GET result poll maps onto the same `Research` surface: a
                // research task is never replayed in either direction.
                name: "research_get_429_is_excluded",
                provider: ProviderId::Tavily,
                request_kind: Kind::Research,
                signal: Signal::RateLimited {
                    retry_after_secs: None,
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                name: "research_post_500_reports_surface_exclusion_first",
                provider: ProviderId::Tavily,
                request_kind: Kind::Research,
                signal: Signal::ServerTransient,
                effective_status: Some(500),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                name: "crawl_429_is_excluded",
                provider: ProviderId::Tavily,
                request_kind: Kind::Crawl,
                signal: Signal::RateLimited {
                    retry_after_secs: Some(0),
                },
                effective_status: Some(429),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: Some(0),
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                name: "crawl_432_is_excluded",
                provider: ProviderId::Tavily,
                request_kind: Kind::Crawl,
                signal: Signal::Exhausted,
                effective_status: Some(432),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                name: "crawl_500_reports_surface_exclusion_first",
                provider: ProviderId::Tavily,
                request_kind: Kind::Crawl,
                signal: Signal::ServerTransient,
                effective_status: Some(500),
                evidence: NO_EVIDENCE,
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestKindExcluded),
            },
            EligibilityCase {
                // Exclusion beats the amendment's transport arm too: a research
                // task may already be running upstream, so it is never replayed.
                name: "research_transport_timeout_reports_surface_exclusion_first",
                provider: ProviderId::Tavily,
                request_kind: Kind::Research,
                signal: Signal::ServerTransient,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Timeout),
                armed_retry_after_secs: None,
                expected: Some(Reason::RequestKindExcluded),
            },
        ]
    }

    #[test]
    fn cross_key_retry_eligibility_table_matches_the_amended_whitelist() {
        let cases = eligibility_cases();
        assert!(
            cases.len() >= 16,
            "plan todo 4 requires a >=12 row table plus >=4 amendment rows, got {}",
            cases.len()
        );
        std::println!(
            "[task-4][manual-qa] eligibility table size = {} rows",
            cases.len()
        );

        for case in &cases {
            let actual = key_retry_ineligibility(
                case.provider,
                case.request_kind,
                case.signal,
                case.effective_status,
                &case.evidence,
                case.armed_retry_after_secs,
            );
            assert_eq!(actual, case.expected, "eligibility row `{}`", case.name);
            assert_eq!(
                is_retry_eligible(
                    case.provider,
                    case.request_kind,
                    case.signal,
                    case.effective_status,
                    &case.evidence,
                    case.armed_retry_after_secs,
                ),
                case.expected.is_none(),
                "predicate row `{}`",
                case.name
            );
            // `std::println!` on purpose: `runtime_logging` shadows the bare
            // macro crate-wide and routes it into the log sink, so a bare
            // `println!` would never reach the `--nocapture` QA transcript.
            std::println!(
                "[task-4][manual-qa] row {} | provider={} surface={} signal={:?} status={} evidence={:?} armed_retry_after={} -> eligible={} reason={:?}",
                case.name,
                case.provider.as_str(),
                surface_label(case.request_kind),
                case.signal,
                optional_status(case.effective_status),
                case.evidence,
                optional_secs(case.armed_retry_after_secs),
                case.expected.is_none(),
                actual,
            );
        }
    }

    /// Surface label for the QA dump; `/research` covers the POST submission
    /// handler and the GET result poll alike.
    fn surface_label(kind: KeyRetryRequestKind) -> &'static str {
        match kind {
            KeyRetryRequestKind::HttpRead => "tavily-http-read",
            KeyRetryRequestKind::ProviderRestRead => "provider-rest-read",
            KeyRetryRequestKind::Research => "/research(POST+GET)",
            KeyRetryRequestKind::Crawl => "/crawl",
        }
    }

    fn optional_status(status: Option<u16>) -> String {
        status.map_or_else(|| "none".to_string(), |value| value.to_string())
    }

    fn optional_secs(secs: Option<u64>) -> String {
        secs.map_or_else(|| "none".to_string(), |value| value.to_string())
    }

    struct DecisionCase {
        name: &'static str,
        signal: ProviderFailureSignal,
        effective_status: Option<u16>,
        evidence: KeyRetryAttemptEvidence,
        retry_after_secs: Option<u64>,
        budget: KeyRetryBudget,
        context: KeyRetryContext,
        expected: RetryDecision,
    }

    fn first_hit_context(elapsed: Duration, eligible_key_available: bool) -> KeyRetryContext {
        KeyRetryContext {
            attempts_used: 1,
            elapsed,
            eligible_key_available,
        }
    }

    /// An eligible first hit: Tavily 429 with an armed Retry-After of 0.
    fn rate_limited() -> (ProviderFailureSignal, Option<u16>, Option<u64>) {
        (
            ProviderFailureSignal::RateLimited {
                retry_after_secs: Some(0),
            },
            Some(429),
            Some(0),
        )
    }

    #[test]
    fn cross_key_retry_decision_table_pins_budget_deadline_and_pool_gates() {
        use KeyRetryIneligibleReason as Reason;
        use ServeOriginalReason as Serve;

        let defaults = KeyRetryBudget::defaults();
        let (eligible_signal, eligible_status, eligible_retry_after) = rate_limited();
        let case = |name: &'static str,
                    budget: KeyRetryBudget,
                    context: KeyRetryContext,
                    expected: RetryDecision|
         -> DecisionCase {
            DecisionCase {
                name,
                signal: eligible_signal,
                effective_status: eligible_status,
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: eligible_retry_after,
                budget,
                context,
                expected,
            }
        };

        let cases = vec![
            case(
                "first_hit_429_with_eligible_key_retries",
                defaults,
                first_hit_context(Duration::from_millis(120), true),
                RetryDecision::Retry,
            ),
            case(
                "second_hit_never_retries_again",
                defaults,
                KeyRetryContext {
                    attempts_used: 2,
                    elapsed: Duration::from_millis(400),
                    eligible_key_available: true,
                },
                RetryDecision::ServeOriginal(Serve::Ineligible(Reason::AttemptsExhausted)),
            ),
            case(
                "env_lowered_attempt_budget_disables_the_retry",
                KeyRetryBudget {
                    max_attempts: 1,
                    ..defaults
                },
                first_hit_context(Duration::from_millis(50), true),
                RetryDecision::ServeOriginal(Serve::Ineligible(Reason::AttemptsExhausted)),
            ),
            case(
                "elapsed_at_deadline_serves_original",
                defaults,
                first_hit_context(Duration::from_secs(5), true),
                RetryDecision::ServeOriginal(Serve::DeadlineExceeded),
            ),
            case(
                "elapsed_beyond_deadline_serves_original",
                defaults,
                first_hit_context(Duration::from_secs(9), true),
                RetryDecision::ServeOriginal(Serve::DeadlineExceeded),
            ),
            case(
                "just_inside_deadline_retries",
                defaults,
                first_hit_context(Duration::from_millis(4_900), true),
                RetryDecision::Retry,
            ),
            case(
                "empty_pool_serves_original",
                defaults,
                first_hit_context(Duration::from_millis(80), false),
                RetryDecision::ServeOriginal(Serve::NoEligibleKey),
            ),
            // Gate order pin: the deadline is cheaper to explain than an empty
            // pool, and a caller only probes the store after it.
            case(
                "deadline_wins_over_empty_pool",
                defaults,
                first_hit_context(Duration::from_secs(6), false),
                RetryDecision::ServeOriginal(Serve::DeadlineExceeded),
            ),
            // The amendment's transient arm walks the same verdict path: an
            // eligible 5xx with a candidate key retries exactly once.
            DecisionCase {
                name: "first_hit_500_with_eligible_key_retries",
                signal: ProviderFailureSignal::ServerTransient,
                effective_status: Some(500),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: None,
                budget: defaults,
                context: first_hit_context(Duration::from_millis(700), true),
                expected: RetryDecision::Retry,
            },
            DecisionCase {
                name: "first_hit_transport_timeout_with_eligible_key_retries",
                signal: ProviderFailureSignal::ServerTransient,
                effective_status: None,
                evidence: KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Timeout),
                retry_after_secs: None,
                budget: defaults,
                context: first_hit_context(Duration::from_millis(1_400), true),
                expected: RetryDecision::Retry,
            },
            DecisionCase {
                // The transient arm obeys the same budget: ONE retry, never two.
                name: "second_hit_500_never_retries_again",
                signal: ProviderFailureSignal::ServerTransient,
                effective_status: Some(500),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: None,
                budget: defaults,
                context: KeyRetryContext {
                    attempts_used: 2,
                    elapsed: Duration::from_millis(900),
                    eligible_key_available: true,
                },
                expected: RetryDecision::ServeOriginal(Serve::Ineligible(
                    Reason::AttemptsExhausted,
                )),
            },
            DecisionCase {
                name: "transient_500_with_empty_pool_serves_original",
                signal: ProviderFailureSignal::ServerTransient,
                effective_status: Some(500),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: None,
                budget: defaults,
                context: first_hit_context(Duration::from_millis(300), false),
                expected: RetryDecision::ServeOriginal(Serve::NoEligibleKey),
            },
            // A request-shaped 4xx short-circuits every budget gate even with a
            // fresh budget and a candidate key waiting: the caller must never
            // mark or rotate the key on this reason (2026-10-16 amendment).
            DecisionCase {
                name: "request_shaped_400_never_rotates_a_key",
                signal: ProviderFailureSignal::ClientError,
                effective_status: Some(400),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: None,
                budget: defaults,
                context: first_hit_context(Duration::from_millis(10), true),
                expected: RetryDecision::ServeOriginal(Serve::Ineligible(
                    Reason::RequestShapedClientError,
                )),
            },
            DecisionCase {
                name: "ineligible_request_error_wins_over_every_budget_gate",
                signal: ProviderFailureSignal::ClientError,
                effective_status: Some(404),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: None,
                budget: defaults,
                context: first_hit_context(Duration::from_secs(30), false),
                expected: RetryDecision::ServeOriginal(Serve::Ineligible(
                    Reason::RequestShapedClientError,
                )),
            },
            // A configured fast path wider than the default admits a longer
            // Retry-After; the closing assertion pins the default refusal.
            DecisionCase {
                name: "configured_fast_path_widens_the_retry_after_gate",
                signal: ProviderFailureSignal::RateLimited {
                    retry_after_secs: Some(8),
                },
                effective_status: Some(429),
                evidence: KeyRetryAttemptEvidence::none(),
                retry_after_secs: Some(8),
                budget: KeyRetryBudget {
                    rate_limit_fast_path: Duration::from_secs(10),
                    ..defaults
                },
                context: first_hit_context(Duration::from_millis(60), true),
                expected: RetryDecision::Retry,
            },
        ];

        std::println!(
            "[task-4][manual-qa] decision table size = {} rows",
            cases.len()
        );
        for case in &cases {
            let actual = decide_cross_key_retry(
                ProviderId::Tavily,
                KeyRetryRequestKind::HttpRead,
                case.signal,
                case.effective_status,
                &case.evidence,
                case.retry_after_secs,
                &case.context,
                &case.budget,
            );
            assert_eq!(actual, case.expected, "decision row `{}`", case.name);
            std::println!(
                "[task-4][manual-qa] decision {} | budget={:?} context={:?} signal={:?} status={} evidence={:?} -> {:?}",
                case.name,
                case.budget,
                case.context,
                case.signal,
                optional_status(case.effective_status),
                case.evidence,
                actual,
            );
        }

        // The widened fast-path row must actually depend on the budget: the
        // same 8s Retry-After is refused by the default 2s window.
        assert_eq!(
            decide_cross_key_retry(
                ProviderId::Tavily,
                KeyRetryRequestKind::HttpRead,
                ProviderFailureSignal::RateLimited {
                    retry_after_secs: Some(8),
                },
                Some(429),
                &KeyRetryAttemptEvidence::none(),
                Some(8),
                &first_hit_context(Duration::from_millis(60), true),
                &defaults,
            ),
            RetryDecision::ServeOriginal(Serve::Ineligible(Reason::RetryAfterBeyondFastPath)),
            "default fast path must refuse an 8s Retry-After"
        );
    }

    #[test]
    fn cross_key_retry_budget_defaults_are_documented_values() {
        let budget = KeyRetryBudget::defaults();
        assert_eq!(budget.max_attempts, 2);
        assert_eq!(budget.deadline, Duration::from_secs(5));
        assert_eq!(budget.rate_limit_fast_path, Duration::from_secs(2));
        assert_eq!(KEY_RETRY_MAX_ATTEMPTS_DEFAULT, 2);
        assert_eq!(KEY_RETRY_DEADLINE_SECS_DEFAULT, 5);
        assert_eq!(KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS_DEFAULT, 2);
    }

    #[test]
    fn cross_key_retry_budget_env_parsing_clamps_and_falls_back() {
        // Missing / blank values keep the defaults.
        assert_eq!(
            parse_key_retry_budget(None, None, None),
            KeyRetryBudget::defaults()
        );
        assert_eq!(
            parse_key_retry_budget(Some("  "), Some(""), None),
            KeyRetryBudget::defaults()
        );

        // Valid overrides land verbatim.
        assert_eq!(
            parse_key_retry_budget(Some("1"), Some("3"), Some("0")),
            KeyRetryBudget {
                max_attempts: 1,
                deadline: Duration::from_secs(3),
                rate_limit_fast_path: Duration::from_secs(0),
            }
        );

        // Garbage and out-of-range values warn and keep the default: a typo
        // must never disable the deadline or buy a second retry.
        assert_eq!(
            parse_key_retry_budget(Some("many"), Some("-1"), Some("2.5")),
            KeyRetryBudget::defaults()
        );
        assert_eq!(
            parse_key_retry_budget(Some("3"), Some("600"), Some("120")),
            KeyRetryBudget::defaults()
        );
        assert_eq!(
            parse_key_retry_budget(Some("2"), Some("60"), Some("60")),
            KeyRetryBudget {
                max_attempts: KEY_RETRY_MAX_ATTEMPTS_CEILING,
                deadline: Duration::from_secs(KEY_RETRY_SECS_CEILING),
                rate_limit_fast_path: Duration::from_secs(KEY_RETRY_SECS_CEILING),
            }
        );
    }

    #[test]
    fn exa_budget_tag_parsing_matches_the_classifier_literals() {
        // Every literal below is the classifier constant itself (plan todo 10
        // made `crate::analysis` the single source of truth), so this test
        // cannot drift away from the tag vocabulary it claims to pin.
        assert_eq!(
            ExaBudgetTag::from_tag(EXA_BUDGET_TAG_NO_MORE_CREDITS),
            Some(ExaBudgetTag::NoMoreCredits)
        );
        let padded = format!(
            " {} ",
            EXA_BUDGET_TAG_API_KEY_BUDGET_EXCEEDED.to_ascii_lowercase()
        );
        assert_eq!(
            ExaBudgetTag::from_tag(&padded),
            Some(ExaBudgetTag::ApiKeyBudgetExceeded)
        );
        assert_eq!(
            ExaBudgetTag::from_tag(EXA_BUDGET_TAG_TEAM_BUDGET_EXCEEDED),
            Some(ExaBudgetTag::TeamBudgetExceeded)
        );
        assert_eq!(ExaBudgetTag::from_tag("OTHER"), None);
        assert_eq!(ExaBudgetTag::from_tag(""), None);
        assert!(ExaBudgetTag::NoMoreCredits.is_key_scoped());
        assert!(ExaBudgetTag::ApiKeyBudgetExceeded.is_key_scoped());
        assert!(!ExaBudgetTag::TeamBudgetExceeded.is_key_scoped());
    }

    /// Manual-QA happy scenario for the 2026-10-16 amendment: an upstream 500
    /// now judges `Retry` on the transient arm instead of being served as-is.
    #[test]
    fn transient_500_retries_on_another_key() {
        let budget = KeyRetryBudget::defaults();
        let context = KeyRetryContext {
            attempts_used: 1,
            elapsed: Duration::from_millis(900),
            eligible_key_available: true,
        };
        let evidence = KeyRetryAttemptEvidence::none();
        let decision = decide_cross_key_retry(
            ProviderId::Tavily,
            KeyRetryRequestKind::HttpRead,
            ProviderFailureSignal::ServerTransient,
            Some(500),
            &evidence,
            None,
            &context,
            &budget,
        );

        std::println!("[task-4][manual-qa] happy scenario: transient upstream 500");
        std::println!(
            "[task-4][manual-qa]   provider={} surface={}",
            ProviderId::Tavily.as_str(),
            surface_label(KeyRetryRequestKind::HttpRead)
        );
        std::println!(
            "[task-4][manual-qa]   signal={:?} effective_status={:?}",
            ProviderFailureSignal::ServerTransient,
            Some(500u16)
        );
        std::println!("[task-4][manual-qa]   evidence={evidence:?} armed_retry_after=None");
        std::println!("[task-4][manual-qa]   budget={budget:?}");
        std::println!("[task-4][manual-qa]   context={context:?}");
        std::println!("[task-4][manual-qa]   decision={decision:?}");
        std::println!(
            "[task-4][manual-qa]   trigger={:?}",
            key_retry_trigger(
                ProviderFailureSignal::ServerTransient,
                Some(500),
                &KeyRetryAttemptEvidence::none()
            )
        );

        assert_eq!(decision, RetryDecision::Retry);
        assert_eq!(
            key_retry_trigger(
                ProviderFailureSignal::ServerTransient,
                Some(500),
                &KeyRetryAttemptEvidence::none()
            ),
            Some(KeyRetryTrigger::Transient),
            "a 500 is a key-scoped transient under the 2026-10-16 amendment"
        );
    }

    /// Manual-QA failure scenario: a request-shaped 400 must produce
    /// `ServeOriginal(RequestShapedClientError)` even with a fresh budget and a
    /// candidate key waiting, so the caller never marks or rotates the key.
    #[test]
    fn request_shaped_400_serves_original_with_reason_payload() {
        let budget = KeyRetryBudget::defaults();
        let context = KeyRetryContext {
            attempts_used: 1,
            elapsed: Duration::from_millis(40),
            eligible_key_available: true,
        };
        let evidence = KeyRetryAttemptEvidence::none();
        let decision = decide_cross_key_retry(
            ProviderId::Tavily,
            KeyRetryRequestKind::HttpRead,
            ProviderFailureSignal::ClientError,
            Some(400),
            &evidence,
            None,
            &context,
            &budget,
        );

        std::println!("[task-4][manual-qa] failure scenario: request-shaped 400");
        std::println!(
            "[task-4][manual-qa]   provider={} surface={}",
            ProviderId::Tavily.as_str(),
            surface_label(KeyRetryRequestKind::HttpRead)
        );
        std::println!(
            "[task-4][manual-qa]   signal={:?} effective_status={:?}",
            ProviderFailureSignal::ClientError,
            Some(400u16)
        );
        std::println!("[task-4][manual-qa]   evidence={evidence:?} armed_retry_after=None");
        std::println!("[task-4][manual-qa]   budget={budget:?}");
        std::println!("[task-4][manual-qa]   context={context:?}");
        std::println!("[task-4][manual-qa]   decision={decision:?}");
        std::println!(
            "[task-4][manual-qa]   trigger={:?}",
            key_retry_trigger(
                ProviderFailureSignal::ClientError,
                Some(400),
                &KeyRetryAttemptEvidence::none()
            )
        );

        assert_eq!(
            decision,
            RetryDecision::ServeOriginal(ServeOriginalReason::Ineligible(
                KeyRetryIneligibleReason::RequestShapedClientError
            ))
        );
        assert!(!matches!(decision, RetryDecision::Retry));
        assert_eq!(
            key_retry_trigger(
                ProviderFailureSignal::ClientError,
                Some(400),
                &KeyRetryAttemptEvidence::none()
            ),
            None,
            "a request-shaped 400 has no retry trigger at all"
        );
    }

    /// Adversarial `malformed_input` probe: garbage Exa tag text and garbage
    /// status codes must never panic and must never turn a request-shaped error
    /// into a retry. The kernel parses no body and no header itself - the only
    /// free text it accepts is the Exa tag the caller already extracted - so
    /// this walks every value a hostile or broken upstream could smuggle in.
    #[test]
    fn garbage_evidence_never_panics_and_never_retries_request_errors() {
        use KeyRetryIneligibleReason as Reason;

        // Garbage tag text resolves to `None`, which keeps an Exa 402 vetoed;
        // the two case-variants that DO parse are pinned as key-scoped. The
        // tag-shaped values are derived from the classifier constants, so a
        // renamed upstream tag cannot leave a stale literal behind here.
        let oversized = "T".repeat(4096);
        let near_miss = format!("{EXA_BUDGET_TAG_NO_MORE_CREDITS}_X");
        let json_wrapped = format!("{{\"tag\":\"{EXA_BUDGET_TAG_TEAM_BUDGET_EXCEEDED}\"}}");
        let lower_team = EXA_BUDGET_TAG_TEAM_BUDGET_EXCEEDED.to_ascii_lowercase();
        let lower_credits = EXA_BUDGET_TAG_NO_MORE_CREDITS.to_ascii_lowercase();
        let tags: [&str; 10] = [
            "",
            "   ",
            "\t\n",
            "\0\0nul-bytes",
            "429",
            near_miss.as_str(),
            json_wrapped.as_str(),
            lower_team.as_str(),
            lower_credits.as_str(),
            oversized.as_str(),
        ];
        for tag in tags {
            let parsed = ExaBudgetTag::from_tag(tag);
            // `escape_debug` keeps control bytes out of the QA transcript: a raw
            // NUL would make the captured log a binary file for every reader.
            let shown = tag.escape_debug().take(200).collect::<String>();
            let evidence = match parsed {
                Some(parsed) => KeyRetryAttemptEvidence::exa_budget(parsed),
                None => KeyRetryAttemptEvidence::none(),
            };
            let reason = key_retry_ineligibility(
                ProviderId::Exa,
                KeyRetryRequestKind::ProviderRestRead,
                ProviderFailureSignal::Exhausted,
                Some(402),
                &evidence,
                None,
            );
            std::println!(
                "[task-4][malformed-input] exa 402 tag len={} shown={shown} -> parsed={parsed:?} reason={reason:?}",
                tag.len()
            );
            let key_scoped = matches!(
                parsed,
                Some(ExaBudgetTag::NoMoreCredits | ExaBudgetTag::ApiKeyBudgetExceeded)
            );
            assert_eq!(
                reason,
                if key_scoped {
                    None
                } else {
                    Some(Reason::ProviderVeto)
                },
                "exa 402 tag {shown} veto mismatch"
            );
        }

        // Garbage status codes. Two properties must hold for every one of them:
        // a request-shaped 4xx is never retryable whatever else the caller
        // thinks it saw, and a code outside the whitelist and outside 5xx has no
        // trigger at all. A `ServerTransient` signal keeps precedence over a
        // bogus status because it is the classifier's own transient claim.
        let fresh_context = KeyRetryContext {
            attempts_used: 1,
            elapsed: Duration::from_millis(5),
            eligible_key_available: true,
        };
        let defaults = KeyRetryBudget::defaults();
        let statuses: [u16; 24] = [
            0, 1, 99, 100, 199, 200, 204, 299, 301, 302, 399, 400, 404, 405, 406, 418, 422, 425,
            451, 499, 500, 599, 600, 65535,
        ];
        for status in statuses {
            for signal in [
                ProviderFailureSignal::Ok,
                ProviderFailureSignal::ClientError,
                ProviderFailureSignal::ServerTransient,
            ] {
                let evidence = KeyRetryAttemptEvidence::none();
                let reason = key_retry_ineligibility(
                    ProviderId::Tavily,
                    KeyRetryRequestKind::HttpRead,
                    signal,
                    Some(status),
                    &evidence,
                    None,
                );
                let decision = decide_cross_key_retry(
                    ProviderId::Tavily,
                    KeyRetryRequestKind::HttpRead,
                    signal,
                    Some(status),
                    &evidence,
                    None,
                    &fresh_context,
                    &defaults,
                );
                std::println!(
                    "[task-4][malformed-input] status={status} signal={signal:?} -> reason={reason:?} decision={decision:?}"
                );
                let request_shaped =
                    matches!(status, 400..=499) && !matches!(status, 401 | 402 | 403 | 429 | 432);
                let whitelisted = matches!(status, 401 | 402 | 403 | 429 | 432 | 500..=599);
                if request_shaped {
                    assert_eq!(
                        reason,
                        Some(Reason::RequestShapedClientError),
                        "status {status} signal {signal:?} must stay request-shaped"
                    );
                    assert!(
                        !matches!(decision, RetryDecision::Retry),
                        "status {status} signal {signal:?} must never rotate a key"
                    );
                } else if !whitelisted && signal != ProviderFailureSignal::ServerTransient {
                    assert_eq!(
                        reason,
                        Some(Reason::NotRetryableOutcome),
                        "status {status} signal {signal:?} has nothing to replay"
                    );
                    assert!(
                        !matches!(decision, RetryDecision::Retry),
                        "status {status} signal {signal:?} must never rotate a key"
                    );
                }
            }
        }

        // A missing status must not panic either, with or without transport
        // evidence attached.
        for evidence in [
            KeyRetryAttemptEvidence::none(),
            KeyRetryAttemptEvidence::transport(KeyRetryTransportFailure::Send),
        ] {
            let reason = key_retry_ineligibility(
                ProviderId::Tavily,
                KeyRetryRequestKind::HttpRead,
                ProviderFailureSignal::ClientError,
                None,
                &evidence,
                None,
            );
            std::println!(
                "[task-4][malformed-input] status=none evidence={evidence:?} -> reason={reason:?}"
            );
            assert_eq!(
                reason,
                if evidence.transport_failure.is_some() {
                    None
                } else {
                    Some(Reason::NotRetryableOutcome)
                }
            );
        }
    }
}
