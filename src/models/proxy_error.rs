use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("invalid upstream endpoint '{endpoint}': {source}")]
    InvalidEndpoint {
        endpoint: String,
        #[source]
        source: url::ParseError,
    },
    #[error("no API keys available in the store")]
    NoAvailableKeys,
    /// Every forward-proxy egress candidate is unavailable, so nothing was sent:
    /// never a transport error, never a reason to replay on another key.
    #[error(
        "no forward-proxy egress available for {provider}: {configured_nodes} configured node(s), none selectable"
    )]
    ForwardProxyEgressUnavailable {
        provider: &'static str,
        configured_nodes: usize,
    },
    #[error("pinned MCP session key is unavailable")]
    PinnedMcpSessionUnavailable,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("missing usage data: {reason}")]
    QuotaDataMissing { reason: String },
    #[error("usage http error {status}: {body}")]
    UsageHttp {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("cannot remove the final admin login method")]
    LastAdminLoginMethod,
    #[error("scheduled job {job_id} claim generation {claim_generation} is stale")]
    StaleClaim { job_id: i64, claim_generation: i64 },
    #[error("deferred {operation}: {reason}")]
    Deferred {
        operation: &'static str,
        reason: String,
    },
    #[error("other error: {0}")]
    Other(String),
}

impl ProxyError {
    pub fn is_stale_claim(&self) -> bool {
        matches!(self, Self::StaleClaim { .. })
    }

    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred { .. })
    }
}
