//! Mock Keenable upstream (REST search/fetch + hosted MCP) for offline tests.
//!
//! The stub mirrors the verified vendor surface: `POST /v1/search`,
//! `GET /v1/fetch`, `POST /mcp` (buffered JSON-RPC 2.0), the `X-API-Key`
//! credential slot, the native success bodies with `usage.credits_remaining`,
//! and the closed `type`/`code` error envelope. Scripted keys drive the failure
//! arms the facades classify in later todos.
//!
//! `GET /metrics` and `GET /last-bodies` are additive observation endpoints:
//! facade suites use them to prove request counts and that no client header
//! leaked upstream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};

const KEY_GOOD: &str = "key-good";
/// Scripted partial arm (keenable-integration todo 7 adversarial): a 200
/// whose body carries NO `usage` object — the facade must still treat it as
/// business success (the SPEC allows `usage` to be absent) while the
/// passive credit harvest skips it cleanly.
const KEY_PARTIAL: &str = "key-partial";
/// Scripted session-migration arm (keenable-integration todo 8): a key
/// revoked mid-session authorizes the initialize but rejects the PINNED
/// follow-up `tools/call` with 401, proving the passthrough's key-migration
/// ladder moves the session onto another pooled key.
const KEY_TURNCOAT: &str = "key-auth-after-init";
const KEY_EXHAUSTED: &str = "key-exhausted";
const KEY_AUTH: &str = "key-auth";
const KEY_RATE: &str = "key-rate";
const KEY_TARGET: &str = "key-target";
const KEY_5XX: &str = "key-5xx";
const KEY_SLOW: &str = "key-slow";
/// Scripted success arm that authenticates and bills normally but omits the
/// documented `X-RateLimit-*` response headers (plan todo 22): the passive
/// rate-limit harvest must write nothing for it.
const KEY_NO_RATE_LIMIT_HEADERS: &str = "key-no-ratelimit-headers";
const STARTING_CREDITS: u64 = 100_000;
const SLOW_DELAY_DEFAULT_MS: u64 = 35_000;

#[derive(Parser)]
#[command(
    name = "mock_keenable",
    about = "Mock Keenable upstream for offline tests"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:58094")]
    bind: String,
    #[arg(
        long,
        env = "MOCK_KEENABLE_KEYS",
        value_delimiter = ',',
        default_value = "mock-keenable-key"
    )]
    keys: Vec<String>,
    /// Delay applied to `key-slow` requests. The default exceeds the facade's
    /// 30s total timeout; tests that only assert the delay pass a smaller knob.
    #[arg(
        long,
        env = "MOCK_KEENABLE_SLOW_DELAY_MS",
        default_value_t = SLOW_DELAY_DEFAULT_MS
    )]
    slow_delay_ms: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum KeyStatus {
    Valid,
    NoRateLimitHeaders,
    Partial,
    Exhausted,
    Invalid,
    RateLimited,
    TargetBlocked,
    Transient,
    Slow,
}

#[derive(Debug, Default)]
struct MockCounters {
    search: AtomicU64,
    fetch: AtomicU64,
    mcp: AtomicU64,
    mcp_initialize: AtomicU64,
    mcp_tools_list: AtomicU64,
    mcp_tools_call: AtomicU64,
    auth_failures: AtomicU64,
    slow_requests: AtomicU64,
}

#[derive(Debug, Default)]
struct LastBodies {
    search: Mutex<Option<String>>,
    fetch: Mutex<Option<String>>,
    mcp: Mutex<Option<String>>,
}

struct MockState {
    keys: Vec<String>,
    slow_delay_ms: u64,
    credits_remaining: AtomicU64,
    counters: MockCounters,
    last_bodies: LastBodies,
    last_api_key: Mutex<Option<String>>,
    /// Raw `Authorization` header value observed on the last keyed request
    /// (None when absent). The hosted-MCP passthrough injects `X-API-Key`, so
    /// the e2e suite asserts this stays absent upstream.
    last_authorization: Mutex<Option<String>>,
}

impl MockState {
    fn new(keys: Vec<String>, slow_delay_ms: u64) -> Self {
        Self {
            keys,
            slow_delay_ms,
            credits_remaining: AtomicU64::new(STARTING_CREDITS),
            counters: MockCounters::default(),
            last_bodies: LastBodies::default(),
            last_api_key: Mutex::new(None),
            last_authorization: Mutex::new(None),
        }
    }

    fn classify(&self, key: &str) -> KeyStatus {
        match key {
            KEY_GOOD => KeyStatus::Valid,
            KEY_NO_RATE_LIMIT_HEADERS => KeyStatus::NoRateLimitHeaders,
            KEY_PARTIAL => KeyStatus::Partial,
            KEY_EXHAUSTED => KeyStatus::Exhausted,
            KEY_AUTH => KeyStatus::Invalid,
            KEY_RATE => KeyStatus::RateLimited,
            KEY_TARGET => KeyStatus::TargetBlocked,
            KEY_5XX => KeyStatus::Transient,
            KEY_SLOW => KeyStatus::Slow,
            _ if self.keys.iter().any(|known| known == key) => KeyStatus::Valid,
            _ => KeyStatus::Invalid,
        }
    }

    /// Charge one credit and return the native `usage` object.
    fn bill(&self, sku: &str) -> Value {
        let previous = self.credits_remaining.fetch_sub(1, Ordering::SeqCst);
        json!({
            "sku": sku,
            "amount": 1,
            "credits": 1,
            "paid": false,
            "credits_remaining": previous.saturating_sub(1)
        })
    }

    /// Documented per-second window (10 rps/org). `X-RateLimit-Reset` is an
    /// ISO 8601 instant per the vendor docs; the fixed stub instant keeps
    /// every harness deterministic.
    fn rate_limit_headers() -> [(HeaderName, String); 4] {
        [
            (
                HeaderName::from_static("x-ratelimit-limit"),
                "10".to_string(),
            ),
            (
                HeaderName::from_static("x-ratelimit-remaining"),
                "9".to_string(),
            ),
            (
                HeaderName::from_static("x-ratelimit-reset"),
                "2026-12-31T23:59:59Z".to_string(),
            ),
            (
                HeaderName::from_static("x-request-id"),
                "mock-request-id".to_string(),
            ),
        ]
    }
}

/// Build a success response with the documented `X-RateLimit-*` triple unless
/// the scripted key suppresses it (plan todo 22 stripped-headers arm).
fn success_response(headers: &HeaderMap, body: Value) -> Response {
    if api_key(headers) == KEY_NO_RATE_LIMIT_HEADERS {
        (StatusCode::OK, Json(body)).into_response()
    } else {
        (StatusCode::OK, MockState::rate_limit_headers(), Json(body)).into_response()
    }
}

fn error_body(error: &str, error_type: &str, code: &str, message: &str, retryable: bool) -> Value {
    json!({
        "error": error,
        "type": error_type,
        "code": code,
        "message": message,
        "request_id": "mock-request-id",
        "retryable": retryable
    })
}

fn invalid_key_body() -> Value {
    error_body(
        "Authentication failed",
        "authentication_error",
        "invalid_api_key",
        "Invalid API key",
        false,
    )
}

fn insufficient_credits_body() -> Value {
    let mut body = error_body(
        "Insufficient credits",
        "billing_error",
        "insufficient_credits",
        "This month's free requests and purchased credits are used up.",
        false,
    );
    body["billing_url"] = json!("https://example.invalid/billing");
    body["auto_top_up_url"] = json!("https://example.invalid/auto-top-up");
    body
}

fn rate_limited_body() -> Value {
    let mut body = error_body(
        "Rate limit exceeded",
        "rate_limit_error",
        "rate_limit_per_second",
        "Too many requests; your organization is limited to 10 requests per second.",
        true,
    );
    body["retry_after_seconds"] = json!(1);
    body["retryAfter"] = json!(1);
    body
}

fn target_blocked_body() -> Value {
    error_body(
        "Upstream forbidden",
        "target_error",
        "target_blocked",
        "The target site refused the fetcher, for example with a bot check.",
        false,
    )
}

fn transient_body() -> Value {
    let mut body = error_body(
        "Search backend failed",
        "api_error",
        "search_upstream_error",
        "Our search backend failed while answering; your request is not the cause.",
        true,
    );
    body["retry_after_seconds"] = json!(1);
    body
}

fn missing_parameter_body(param: &str) -> Value {
    let mut body = error_body(
        "Missing required parameter",
        "invalid_request_error",
        "missing_parameter",
        &format!("\"{param}\" is required"),
        false,
    );
    body["param"] = json!(param);
    body
}

fn remember(slot: &Mutex<Option<String>>, value: Option<&str>) {
    *slot.lock().expect("last body lock") = Some(value.unwrap_or_default().to_string());
}

fn read_json(slot: &Mutex<Option<String>>) -> Value {
    let Some(text) = slot.lock().expect("last body lock").clone() else {
        return Value::Null;
    };
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

fn read_raw(slot: &Mutex<Option<String>>) -> Value {
    slot.lock()
        .expect("last body lock")
        .clone()
        .map_or(Value::Null, Value::String)
}

fn api_key(headers: &HeaderMap) -> String {
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
        .unwrap_or_default()
        .to_string()
}

/// Authenticate the request and apply the scripted failure arm. A valid or
/// slow key returns `None` so the handler continues.
async fn authorize(state: &MockState, headers: &HeaderMap) -> Option<Response> {
    let key = api_key(headers);
    if !key.is_empty() {
        *state.last_api_key.lock().expect("api key lock") = Some(key.clone());
    }
    *state.last_authorization.lock().expect("authorization lock") = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    match state.classify(&key) {
        KeyStatus::Valid | KeyStatus::NoRateLimitHeaders => None,
        KeyStatus::Partial => Some(
            (
                StatusCode::OK,
                Json(json!({
                    "query": "partial",
                    "results": [],
                    "partial": true
                })),
            )
                .into_response(),
        ),
        KeyStatus::Slow => {
            state.counters.slow_requests.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(state.slow_delay_ms)).await;
            None
        }
        KeyStatus::Invalid => {
            state.counters.auth_failures.fetch_add(1, Ordering::Relaxed);
            Some((StatusCode::UNAUTHORIZED, Json(invalid_key_body())).into_response())
        }
        KeyStatus::Exhausted => Some(
            (
                StatusCode::PAYMENT_REQUIRED,
                Json(insufficient_credits_body()),
            )
                .into_response(),
        ),
        KeyStatus::RateLimited => Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "1")],
                Json(rate_limited_body()),
            )
                .into_response(),
        ),
        KeyStatus::TargetBlocked => {
            Some((StatusCode::FORBIDDEN, Json(target_blocked_body())).into_response())
        }
        KeyStatus::Transient => {
            Some((StatusCode::SERVICE_UNAVAILABLE, Json(transient_body())).into_response())
        }
    }
}

fn search_success_body(state: &MockState, query: &str, mode: &str) -> Value {
    json!({
        "query": query,
        "mode": mode,
        "results": [
            {
                "title": "Mock Keenable Result",
                "url": "https://example.com/1",
                "description": "mock description",
                "snippet": "mock snippet",
                "published_at": "2026-01-01"
            }
        ],
        "usage": state.bill("search")
    })
}

fn fetch_success_body(state: &MockState, url: &str) -> Value {
    let content = "mock page content";
    json!({
        "url": url,
        "title": "Mock Page",
        "content": content,
        "description": "",
        "truncated": false,
        "total_chars": content.len(),
        "usage": state.bill("fetch")
    })
}

async fn handle_search(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Option<String>,
) -> Response {
    state.counters.search.fetch_add(1, Ordering::Relaxed);
    remember(&state.last_bodies.search, body.as_deref());
    if let Some(response) = authorize(&state, &headers).await {
        return response;
    }
    let raw = body.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let query = parsed
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(missing_parameter_body("query")),
        )
            .into_response();
    }
    let mode = parsed.get("mode").and_then(Value::as_str).unwrap_or("pro");
    success_response(&headers, search_success_body(&state, query, mode))
}

#[derive(Deserialize)]
struct FetchQuery {
    url: Option<String>,
}

async fn handle_fetch(
    State(state): State<Arc<MockState>>,
    Query(query): Query<FetchQuery>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Response {
    state.counters.fetch.fetch_add(1, Ordering::Relaxed);
    remember(&state.last_bodies.fetch, raw_query.as_deref());
    if let Some(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(url) = query.url.as_deref().filter(|url| !url.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(missing_parameter_body("url"))).into_response();
    };
    success_response(&headers, fetch_success_body(&state, url))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .into_response()
}

fn rpc_result(id: Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn mcp_tool(name: &str, description: &str, property: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                property: { "type": "string", "description": format!("{name} {property}") }
            },
            "required": [property]
        }
    })
}

fn tools_call_result(
    state: &MockState,
    name: &str,
    arguments: &Value,
) -> Result<Value, Box<Response>> {
    let native = match name {
        "search_web_pages" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if query.trim().is_empty() {
                return Err(Box::new(rpc_error(
                    Value::Null,
                    -32602,
                    "search_web_pages requires a non-empty query",
                )));
            }
            let mode = arguments
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("pro");
            search_success_body(state, query, mode)
        }
        "fetch_page_content" => {
            let url = arguments
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if url.trim().is_empty() {
                return Err(Box::new(rpc_error(
                    Value::Null,
                    -32602,
                    "fetch_page_content requires a non-empty url",
                )));
            }
            fetch_success_body(state, url)
        }
        _ => {
            return Err(Box::new(rpc_error(
                Value::Null,
                -32602,
                &format!("unknown tool {name}"),
            )));
        }
    };
    let usage = native.get("usage").cloned().unwrap_or(Value::Null);
    let encoded = serde_json::to_string(&native).unwrap_or_default();
    Ok(json!({
        "content": [{ "type": "text", "text": encoded }],
        "structuredContent": native,
        "_meta": { "keenable/usage": usage }
    }))
}

async fn handle_mcp(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Option<String>,
) -> Response {
    state.counters.mcp.fetch_add(1, Ordering::Relaxed);
    remember(&state.last_bodies.mcp, body.as_deref());
    let key = api_key(&headers);
    if let Some(response) = authorize(&state, &headers).await {
        return response;
    }
    let raw = body.unwrap_or_default();
    let Ok(request) = serde_json::from_str::<Value>(&raw) else {
        return rpc_error(Value::Null, -32700, "Parse error");
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "initialize" => {
            state
                .counters
                .mcp_initialize
                .fetch_add(1, Ordering::Relaxed);
            // Vendor-faithful JSON-RPC envelope: the probe captures for the
            // real hosted server wrap the initialize result under `result`.
            // The mock used to answer a bare object, which the mock contract
            // test (and the vendor shape) rejects.
            let body = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                    "serverInfo": {
                        "name": "keenable-mcp-server",
                        "version": "0.2.1",
                        "title": "Keenable Web Search"
                    }
                }
            });
            // Streamable-HTTP lifecycle: initialize mints the upstream protocol
            // session id the passthrough translates into a proxy session id
            // (mirrors the vendor's hosted MCP session behavior).
            (
                StatusCode::OK,
                [("mcp-session-id", "mock-keenable-mcp-session")],
                Json(body),
            )
                .into_response()
        }
        "tools/list" => {
            state
                .counters
                .mcp_tools_list
                .fetch_add(1, Ordering::Relaxed);
            rpc_result(
                id,
                json!({
                    "tools": [
                        mcp_tool(
                            "search_web_pages",
                            "Search the live web with Keenable.",
                            "query"
                        ),
                        mcp_tool(
                            "fetch_page_content",
                            "Read any page as clean markdown.",
                            "url"
                        )
                    ]
                }),
            )
        }
        "tools/call" => {
            state
                .counters
                .mcp_tools_call
                .fetch_add(1, Ordering::Relaxed);
            if key == KEY_TURNCOAT {
                state.counters.auth_failures.fetch_add(1, Ordering::Relaxed);
                return (StatusCode::UNAUTHORIZED, Json(invalid_key_body())).into_response();
            }
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            match tools_call_result(&state, name, &arguments) {
                Ok(result) => success_response(
                    &headers,
                    json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                ),
                Err(response) => *response,
            }
        }
        "ping" => rpc_result(id, json!({})),
        notification if notification.starts_with("notifications/") => {
            StatusCode::ACCEPTED.into_response()
        }
        _ => rpc_error(id, -32601, "Method not found"),
    }
}

async fn handle_metrics(State(state): State<Arc<MockState>>) -> Json<Value> {
    Json(json!({
        "search": state.counters.search.load(Ordering::Relaxed),
        "fetch": state.counters.fetch.load(Ordering::Relaxed),
        "mcp": state.counters.mcp.load(Ordering::Relaxed),
        "mcp_initialize": state.counters.mcp_initialize.load(Ordering::Relaxed),
        "mcp_tools_list": state.counters.mcp_tools_list.load(Ordering::Relaxed),
        "mcp_tools_call": state.counters.mcp_tools_call.load(Ordering::Relaxed),
        "auth_failures": state.counters.auth_failures.load(Ordering::Relaxed),
        "slow_requests": state.counters.slow_requests.load(Ordering::Relaxed),
        "credits_remaining": state.credits_remaining.load(Ordering::Relaxed),
        "last_api_key": state.last_api_key.lock().expect("api key lock").clone(),
        "last_authorization": state
            .last_authorization
            .lock()
            .expect("authorization lock")
            .clone()
    }))
}

async fn handle_last_bodies(State(state): State<Arc<MockState>>) -> Json<Value> {
    Json(json!({
        "search": read_json(&state.last_bodies.search),
        "fetch": read_raw(&state.last_bodies.fetch),
        "mcp": read_json(&state.last_bodies.mcp)
    }))
}

fn build_router(state: Arc<MockState>) -> Router {
    Router::new()
        .route("/v1/search", post(handle_search))
        .route("/v1/fetch", get(handle_fetch))
        .route("/mcp", post(handle_mcp))
        .route("/metrics", get(handle_metrics))
        .route("/last-bodies", get(handle_last_bodies))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let state = Arc::new(MockState::new(cli.keys, cli.slow_delay_ms));
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
    println!(
        "Mock Keenable upstream listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;
    Ok(())
}
