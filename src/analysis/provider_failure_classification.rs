use super::analyze_json_message;
use crate::models::*;
use crate::providers::ProviderId;
use crate::*;

// Provider failure signals (plan todo 3); consumed by the provider facades and health wiring (todos 11-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFailureSignal {
    Ok,
    Exhausted,
    AuthFailed,
    RateLimited { retry_after_secs: Option<u64> },
    ClientError,
    ServerTransient,
}

impl ProviderFailureSignal {
    pub(crate) fn failure_kind(self) -> Option<&'static str> {
        match self {
            Self::Ok | Self::Exhausted => None,
            Self::AuthFailed => Some(FAILURE_KIND_UPSTREAM_ACCOUNT_DEACTIVATED_401),
            Self::RateLimited { .. } => Some(FAILURE_KIND_UPSTREAM_RATE_LIMITED_429),
            Self::ClientError => Some(FAILURE_KIND_OTHER),
            Self::ServerTransient => Some(FAILURE_KIND_UPSTREAM_GATEWAY_5XX),
        }
    }
}

/// Typed transport-failure classification for an audit row that never got an
/// upstream response.
///
/// The store's fallback (`classify_failure_kind`) reads the SAME diagnosis back
/// out of the error's `Display` text, so a wording change there would silently
/// downgrade every row to `transport_send_error` - exactly the misdiagnosis the
/// 2026-10-22 Brave audit had to unwind by hand. Rows that already hold the
/// typed error therefore stamp the kind themselves and let `Display` stay prose.
///
/// `None` keeps today's behavior for variants whose kind is genuinely decided
/// by status/body rather than by the error alone.
///
/// The `Http` arm is deliberately narrow: only a failure that never got a
/// response (connect / request-send) is transport evidence. A body or decode
/// error carries a different diagnosis and must keep falling through to the
/// classifier instead of being relabelled as a send failure.
pub(crate) fn failure_kind_from_proxy_error(err: &ProxyError) -> Option<&'static str> {
    match err {
        ProxyError::ForwardProxyEgressUnavailable { .. } => {
            Some(FAILURE_KIND_FORWARD_PROXY_EGRESS_UNAVAILABLE)
        }
        ProxyError::Http(source) if source.is_connect() || source.is_request() => {
            Some(FAILURE_KIND_TRANSPORT_SEND_ERROR)
        }
        _ => None,
    }
}

/// Key-migration signal for the bin-side MCP follow-up ladders (plan
/// provider-key-429-retry todo 13).
///
/// The ladders live in the binary crate, where the classifier internals are
/// crate-private, so this is the same boundary projection pattern as
/// `serpapi_facade_outcome`: ONE call maps a buffered MCP follow-up response
/// onto the signal the ladder branches on. `RateLimited` is reported for
/// completeness only - both ladders keep their existing 429 detection
/// (`upstream_rate_limited_429`) and the "wait Retry-After, retry once on the
/// SAME key" behavior it drives, untouched; only `AuthFailed` / `Exhausted`
/// migrate a pinned session onto another key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpKeyMigrationSignal {
    /// Not a key-scoped rejection: the attempt is served to the client as-is.
    NotEligible,
    /// Throughput 429: the ladder's Retry-After wait + same-key retry owns it.
    RateLimited,
    /// 401/403 auth rejection: the pinned key is dead.
    AuthFailed,
    /// Quota run-out (Tavily 432, SerpApi "run out of searches"): the pinned
    /// key is spent.
    Exhausted,
}

impl McpKeyMigrationSignal {
    /// True exactly for the two signals that justify moving a pinned session
    /// onto another key.
    pub fn requires_key_migration(self) -> bool {
        matches!(self, Self::AuthFailed | Self::Exhausted)
    }
}

/// Classify one buffered MCP follow-up response for the session-migration
/// branch. `effective_status` is the status the CALLER already derived, which
/// keeps this off the hot path: the Tavily `/mcp` ladder passes
/// `analyze_mcp_attempt(..).tavily_status_code` (that analyzer lifts an
/// envelope-shaped `status: 432` out of an HTTP 200 body) falling back to the
/// HTTP status, and the hosted provider passthrough passes the HTTP status
/// itself. `body` is only read where a provider's verdict depends on it -
/// SerpApi's dual-semantic 429, whose run-out flavor travels as text in either
/// the raw-JSON or the SSE-envelope shape. Retry-After sizing is irrelevant
/// here (each ladder sizes its own wait), so no headers are passed.
pub fn mcp_key_migration_signal(
    provider: ProviderId,
    effective_status: u16,
    body: &[u8],
) -> McpKeyMigrationSignal {
    match classify_provider_failure(
        provider,
        &ProviderFailureEvidence {
            status: effective_status,
            headers: None,
            body,
        },
    ) {
        ProviderFailureSignal::RateLimited { .. } => McpKeyMigrationSignal::RateLimited,
        ProviderFailureSignal::AuthFailed => McpKeyMigrationSignal::AuthFailed,
        ProviderFailureSignal::Exhausted => McpKeyMigrationSignal::Exhausted,
        ProviderFailureSignal::Ok
        | ProviderFailureSignal::ClientError
        | ProviderFailureSignal::ServerTransient => McpKeyMigrationSignal::NotEligible,
    }
}

/// Exhaustion depletion gate (M8): Tavily-432 ONLY; new providers never write
/// depletion rows (todo 9 threaded provider-scoped leases).
pub(crate) fn low_quota_depletion_applies(
    provider: ProviderId,
    tavily_status_code: Option<i64>,
) -> bool {
    provider == ProviderId::Tavily && tavily_status_code == Some(432)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderFailureEvidence<'a> {
    pub(crate) status: u16,
    pub(crate) headers: Option<&'a HeaderMap>,
    pub(crate) body: &'a [u8],
}

pub(crate) fn classify_provider_failure(
    provider: ProviderId,
    evidence: &ProviderFailureEvidence<'_>,
) -> ProviderFailureSignal {
    match provider {
        ProviderId::Tavily => classify_tavily_failure(evidence),
        ProviderId::Serpapi => classify_serpapi_failure(evidence),
        ProviderId::Exa => classify_exa_failure(evidence),
        ProviderId::Brave => classify_brave_failure(evidence),
    }
}

fn default_provider_failure_signal(status: u16) -> ProviderFailureSignal {
    if (200..300).contains(&status) {
        ProviderFailureSignal::Ok
    } else if status >= 500 {
        ProviderFailureSignal::ServerTransient
    } else {
        ProviderFailureSignal::ClientError
    }
}

fn retry_after_header_secs(headers: Option<&HeaderMap>) -> Option<u64> {
    let value = headers?.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok()
}

/// Test-only helper: the production Brave harvest parses the month window
/// via `parse_brave_rate_limit_month_window` (positional X-RateLimit-Policy
/// pairing). Kept for header-format unit pins; not wired into facades.
#[allow(dead_code)]
pub(crate) fn brave_rate_limit_reset_secs(headers: Option<&HeaderMap>) -> Option<i64> {
    let value = headers?.get("x-ratelimit-reset")?.to_str().ok()?;
    value.trim().parse::<i64>().ok()
}

fn classify_tavily_failure(evidence: &ProviderFailureEvidence<'_>) -> ProviderFailureSignal {
    match evidence.status {
        432 => ProviderFailureSignal::Exhausted,
        429 => ProviderFailureSignal::RateLimited {
            retry_after_secs: retry_after_header_secs(evidence.headers),
        },
        401 | 403 => ProviderFailureSignal::AuthFailed,
        _ => default_provider_failure_signal(evidence.status),
    }
}

/// SerpApi 429 DUAL-semantic: "run out of searches" (raw JSON or SSE frame)
/// exhausts; any other message is transient throughput, NEVER exhaustion.
fn classify_serpapi_failure(evidence: &ProviderFailureEvidence<'_>) -> ProviderFailureSignal {
    match evidence.status {
        401 | 403 => ProviderFailureSignal::AuthFailed,
        429 => {
            if upstream_error_texts(evidence.body)
                .iter()
                .any(|text| text.to_ascii_lowercase().contains("run out of searches"))
            {
                ProviderFailureSignal::Exhausted
            } else {
                ProviderFailureSignal::RateLimited {
                    retry_after_secs: retry_after_header_secs(evidence.headers),
                }
            }
        }
        _ => default_provider_failure_signal(evidence.status),
    }
}

fn upstream_error_texts(body: &[u8]) -> Vec<String> {
    let mut messages = Vec::new();
    if let Ok(parsed) = serde_json::from_slice::<Value>(body) {
        messages.push(parsed);
    }
    if let Ok(text) = std::str::from_utf8(body) {
        messages.extend(extract_sse_json_messages(text));
    }
    let mut texts = Vec::new();
    for message in &messages {
        push_upstream_error_texts(message, &mut texts);
    }
    texts
}

fn push_upstream_error_texts(value: &Value, out: &mut Vec<String>) {
    match value.get("error") {
        Some(Value::String(message)) => out.push(message.clone()),
        Some(error @ Value::Object(_)) => {
            if let Some(message) = error.get("message").and_then(Value::as_str) {
                out.push(message.to_string());
            }
            if let Some(data) = error.get("data") {
                if let Some(message) = data.as_str() {
                    out.push(message.to_string());
                }
                push_upstream_error_texts(data, out);
            }
        }
        _ => {}
    }
}

/// Exa 402 budget tags exactly as the upstream envelope spells them
/// (`docs/multi-provider-search-proxy.md` §4.2). Single source of truth for
/// these literals: the retry kernel and its tests reference the constants and
/// [`ExaBudgetTag::from_tag`] instead of restating the strings.
pub(crate) const EXA_BUDGET_TAG_NO_MORE_CREDITS: &str = "NO_MORE_CREDITS";
pub(crate) const EXA_BUDGET_TAG_API_KEY_BUDGET_EXCEEDED: &str = "API_KEY_BUDGET_EXCEEDED";
pub(crate) const EXA_BUDGET_TAG_TEAM_BUDGET_EXCEEDED: &str = "TEAM_BUDGET_EXCEEDED";

/// Which budget an Exa 402 reports as spent (plan todo 10).
///
/// All three tags keep classifying as [`ProviderFailureSignal::Exhausted`], so
/// every existing mark/arm consumer sees exactly what it saw before; the
/// KEY-vs-TEAM *scope* rides beside the signal as structured evidence for the
/// cross-key retry kernel, whose Exa veto replays a key-scoped budget on
/// another key and never replays a team-scoped one (another key cannot lift a
/// team budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExaBudgetTag {
    /// Prepaid account credits ran out.
    NoMoreCredits,
    /// This key's own budget cap was hit.
    ApiKeyBudgetExceeded,
    /// The team-wide budget cap was hit: no other key can serve the request.
    TeamBudgetExceeded,
}

impl ExaBudgetTag {
    /// Parse an upstream `tag` value. Whitespace- and case-insensitive; any
    /// other tag yields `None`, which never means exhaustion.
    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_uppercase().as_str() {
            EXA_BUDGET_TAG_NO_MORE_CREDITS => Some(Self::NoMoreCredits),
            EXA_BUDGET_TAG_API_KEY_BUDGET_EXCEEDED => Some(Self::ApiKeyBudgetExceeded),
            EXA_BUDGET_TAG_TEAM_BUDGET_EXCEEDED => Some(Self::TeamBudgetExceeded),
            _ => None,
        }
    }

    /// True when the budget that ran out belongs to the KEY, so another key can
    /// serve the request; a team-scoped budget cannot be escaped by switching.
    pub(crate) fn is_key_scoped(self) -> bool {
        !matches!(self, Self::TeamBudgetExceeded)
    }
}

/// Structured Exa 402 veto evidence: the budget tag the body carried, or `None`
/// when it carried no parsable budget tag (an untagged 402 is a client error,
/// and a missing tag keeps the kernel's conservative veto). A facade caller
/// feeds this straight into `KeyRetryAttemptEvidence::exa_budget_tag`, which is
/// what keeps the retry kernel zero-I/O - it never parses a body itself.
pub(crate) fn exa_budget_tag_evidence(body: &[u8]) -> Option<ExaBudgetTag> {
    json_string_field(body, &["tag"]).and_then(|tag| ExaBudgetTag::from_tag(&tag))
}

fn classify_exa_failure(evidence: &ProviderFailureEvidence<'_>) -> ProviderFailureSignal {
    match evidence.status {
        401 => ProviderFailureSignal::AuthFailed,
        // Any budget tag exhausts, exactly as before; the tag's KEY-vs-TEAM
        // scope is surfaced separately by `exa_budget_tag_evidence` for the
        // retry veto, so `Exhausted` keeps one meaning for existing consumers.
        402 if exa_budget_tag_evidence(evidence.body).is_some() => ProviderFailureSignal::Exhausted,
        402 => ProviderFailureSignal::ClientError,
        429 => ProviderFailureSignal::RateLimited {
            retry_after_secs: retry_after_header_secs(evidence.headers),
        },
        _ => default_provider_failure_signal(evidence.status),
    }
}

fn json_string_field(body: &[u8], path: &[&str]) -> Option<String> {
    let parsed = serde_json::from_slice::<Value>(body).ok()?;
    let (last, parents) = path.split_last()?;
    let parent = parents
        .iter()
        .try_fold(&parsed, |value, key| value.get(key))?;
    parent
        .get(*last)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Brave envelope `{error:{code,...}}`. Live-probed: INVALID token = 422
/// SUBSCRIPTION_TOKEN_INVALID (NOT 401); VALIDATION promotes to AuthFailed
/// only when meta.errors loc = ["header","x-subscription-token"].
fn classify_brave_failure(evidence: &ProviderFailureEvidence<'_>) -> ProviderFailureSignal {
    let rate_limited = || ProviderFailureSignal::RateLimited {
        retry_after_secs: retry_after_header_secs(evidence.headers),
    };
    if let Some(code) = json_string_field(evidence.body, &["error", "code"]) {
        match code.to_ascii_uppercase().as_str() {
            "SUBSCRIPTION_TOKEN_INVALID" => return ProviderFailureSignal::AuthFailed,
            "QUOTA_LIMITED" => return ProviderFailureSignal::Exhausted,
            "RATE_LIMITED" => return rate_limited(),
            "VALIDATION" => {
                return if brave_validation_flags_subscription_token(evidence.body) {
                    ProviderFailureSignal::AuthFailed
                } else {
                    ProviderFailureSignal::ClientError
                };
            }
            _ => {}
        }
    }
    match evidence.status {
        401 => ProviderFailureSignal::AuthFailed,
        402 => ProviderFailureSignal::Exhausted,
        429 => rate_limited(),
        _ => default_provider_failure_signal(evidence.status),
    }
}

fn brave_validation_flags_subscription_token(body: &[u8]) -> bool {
    let Ok(parsed) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(errors) = parsed
        .get("error")
        .and_then(|error| error.get("meta"))
        .and_then(|meta| meta.get("errors"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    errors.iter().any(|entry| {
        entry
            .get("loc")
            .and_then(Value::as_array)
            .is_some_and(|loc| {
                let [place, name, ..] = loc.as_slice() else {
                    return false;
                };
                place
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case("header"))
                    && name
                        .as_str()
                        .is_some_and(|value| value.eq_ignore_ascii_case("x-subscription-token"))
            })
    })
}

/// Best-effort detection of whether a Tavily MCP response contains *any* error.
///
/// This is used by downstream billing code to avoid over-charging when a JSON-RPC batch
/// contains partial failures (e.g. some items succeed but others error/quota-exhaust).
///
/// Conservative behavior: if we cannot confidently parse the response, treat it as "has error"
/// so we never apply the "expected credits" billing fallback on ambiguous payloads.
pub fn mcp_response_has_any_error(body: &[u8]) -> bool {
    let text = match std::str::from_utf8(body) {
        Ok(text) => text,
        Err(_) => return true,
    };

    let mut messages = extract_sse_json_messages(text);
    if messages.is_empty()
        && let Ok(value) = serde_json::from_str::<Value>(text)
    {
        match value {
            Value::Array(items) => messages.extend(items),
            other => messages.push(other),
        }
    }

    if messages.is_empty() {
        return true;
    }

    for message in messages {
        let Some((outcome, _code)) = analyze_json_message(&message) else {
            return true;
        };
        if outcome != MessageOutcome::Success {
            return true;
        }
    }

    false
}

/// Best-effort detection of whether a Tavily MCP response contains at least one successful item.
pub fn mcp_response_has_any_success(body: &[u8]) -> bool {
    let text = match std::str::from_utf8(body) {
        Ok(text) => text,
        Err(_) => return false,
    };

    let mut messages = extract_sse_json_messages(text);
    if messages.is_empty()
        && let Ok(value) = serde_json::from_str::<Value>(text)
    {
        match value {
            Value::Array(items) => messages.extend(items),
            other => messages.push(other),
        }
    }

    if messages.is_empty() {
        return false;
    }

    for message in messages {
        if let Some((outcome, _code)) = analyze_json_message(&message)
            && outcome == MessageOutcome::Success
        {
            return true;
        }
    }

    false
}

pub(crate) fn extract_sse_json_messages(text: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !current.is_empty() {
                if let Ok(value) = serde_json::from_str::<Value>(&current) {
                    messages.push(value);
                }
                current.clear();
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("data:") {
            let content = rest.trim_start();
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(content);
        }
    }

    if !current.is_empty()
        && let Ok(value) = serde_json::from_str::<Value>(&current)
    {
        messages.push(value);
    }

    messages
}
