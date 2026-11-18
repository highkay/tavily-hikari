//! Smoke matrix for the `mock_keenable` and `mock_zhihu` stub upstreams.
//!
//! Each test spawns the real binary on an ephemeral port and walks every
//! scripted credential arm, asserting the documented HTTP status, envelope, and
//! observation counters. The printed Markdown table is the QA artifact: it
//! proves each branch was hit, not just compiled.

#[path = "common/support_binaries.rs"]
mod support_binaries;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct MockProcess {
    child: Child,
}

impl Drop for MockProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn a mock on `127.0.0.1:0` and wait for its bound URL line. The mocks
/// print after `bind` succeeds, so the line is the readiness event.
async fn spawn_mock(path: PathBuf, args: &[&str]) -> (MockProcess, String) {
    let mut child = Command::new(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mock upstream");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(url) = line.split("listening on ").nth(1) {
                        let _ = tx.send(url.trim().to_string());
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let url = tokio::time::timeout(Duration::from_secs(15), rx)
        .await
        .expect("mock startup timed out")
        .expect("mock printed no listening line");
    (MockProcess { child }, url)
}

#[derive(Default)]
struct Matrix {
    rows: Vec<String>,
}

impl Matrix {
    fn record(&mut self, mock: &str, case: &str, status: u16, detail: impl AsRef<str>) {
        let row = format!("| {mock} | {case} | {status} | {} |", detail.as_ref());
        println!("{row}");
        self.rows.push(row);
    }

    fn finish(self, expected: usize) {
        println!("\nmatrix_cases={}\n", self.rows.len());
        assert_eq!(self.rows.len(), expected, "every scripted branch ran once");
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .expect("build http client")
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    key: Option<&str>,
    body: &Value,
) -> reqwest::Response {
    let mut request = client.post(url).json(body);
    if let Some(key) = key {
        request = request.header("x-api-key", key);
    }
    request.send().await.expect("request completes")
}

#[tokio::test]
async fn mock_keenable_scripted_branches_and_observation_endpoints() {
    // key-slow is shortened so the delay assertion stays fast; the default
    // (35s) exceeds the facade's 30s total timeout.
    let (_mock, base) = spawn_mock(
        support_binaries::mock_keenable_binary(),
        &["--bind", "127.0.0.1:0", "--slow-delay-ms", "300"],
    )
    .await;
    println!("mock_keenable base={base}");
    let client = client();
    let mut matrix = Matrix::default();

    // key-good search: native body + usage + rate-limit headers.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-good"),
        &json!({ "query": "mock query", "max_results": 3 }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        header(&response, "x-ratelimit-limit").as_deref(),
        Some("10")
    );
    assert!(header(&response, "x-ratelimit-remaining").is_some());
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["query"], json!("mock query"));
    assert_eq!(body["mode"], json!("pro"));
    assert_eq!(body["results"][0]["url"], json!("https://example.com/1"));
    assert_eq!(body["results"][0]["published_at"], json!("2026-01-01"));
    assert_eq!(body["usage"]["sku"], json!("search"));
    assert_eq!(body["usage"]["amount"], json!(1));
    assert!(
        body["usage"]["credits_remaining"]
            .as_u64()
            .expect("credits_remaining is a number")
            < 100_000
    );
    matrix.record(
        "mock_keenable",
        "key-good search",
        200,
        format!(
            "usage.sku=search credits_remaining={}",
            body["usage"]["credits_remaining"]
        ),
    );

    // key-good fetch: native fetch body + usage.
    let response = client
        .get(format!("{base}/v1/fetch"))
        .query(&[("url", "https://example.com"), ("max_chars", "1000")])
        .header("x-api-key", "key-good")
        .send()
        .await
        .expect("request completes");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["url"], json!("https://example.com"));
    assert_eq!(body["title"], json!("Mock Page"));
    assert_eq!(body["truncated"], json!(false));
    assert_eq!(body["usage"]["sku"], json!("fetch"));
    assert!(body["total_chars"].as_u64().expect("chars") > 0);
    matrix.record(
        "mock_keenable",
        "key-good fetch",
        200,
        format!("usage.sku=fetch total_chars={}", body["total_chars"]),
    );

    // key-exhausted: 402 billing_error / insufficient_credits.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-exhausted"),
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("billing_error"));
    assert_eq!(body["code"], json!("insufficient_credits"));
    assert_eq!(body["retryable"], json!(false));
    assert!(body["billing_url"].is_string());
    matrix.record(
        "mock_keenable",
        "key-exhausted search",
        402,
        format!("type={} code={}", body["type"], body["code"]),
    );

    // key-auth: 401 authentication_error / invalid_api_key.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-auth"),
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("authentication_error"));
    assert_eq!(body["code"], json!("invalid_api_key"));
    matrix.record(
        "mock_keenable",
        "key-auth search",
        401,
        format!("type={} code={}", body["type"], body["code"]),
    );

    // Missing key is the same 401 arm, and the auth counter reached 2.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        None,
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let metrics: Value = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request")
        .json()
        .await
        .expect("metrics json");
    assert_eq!(metrics["auth_failures"], json!(2));
    matrix.record(
        "mock_keenable",
        "missing key search",
        401,
        format!("auth_failures={}", metrics["auth_failures"]),
    );

    // key-rate: 429 with Retry-After: 1 and the rate_limit_error envelope.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-rate"),
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&response, "retry-after").as_deref(), Some("1"));
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("rate_limit_error"));
    assert_eq!(body["code"], json!("rate_limit_per_second"));
    assert_eq!(body["retry_after_seconds"], json!(1));
    matrix.record(
        "mock_keenable",
        "key-rate search",
        429,
        format!("retry-after=1 type={} code={}", body["type"], body["code"]),
    );

    // key-target: 403 target_error / target_blocked (never an auth failure).
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-target"),
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("target_error"));
    assert_eq!(body["code"], json!("target_blocked"));
    matrix.record(
        "mock_keenable",
        "key-target search",
        403,
        format!("type={} code={}", body["type"], body["code"]),
    );

    // key-5xx: 503 api_error / search_upstream_error.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-5xx"),
        &json!({ "query": "mock query" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("api_error"));
    assert_eq!(body["code"], json!("search_upstream_error"));
    matrix.record(
        "mock_keenable",
        "key-5xx search",
        503,
        format!("type={} code={}", body["type"], body["code"]),
    );

    // Missing query: 400 invalid_request_error / missing_parameter.
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-good"),
        &json!({ "max_results": 3 }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["type"], json!("invalid_request_error"));
    assert_eq!(body["code"], json!("missing_parameter"));
    assert_eq!(body["param"], json!("query"));
    matrix.record(
        "mock_keenable",
        "key-good search missing query",
        400,
        format!("code={} param={}", body["code"], body["param"]),
    );

    // key-slow: the response is delayed by the configured knob.
    let started = Instant::now();
    let response = post_json(
        &client,
        &format!("{base}/v1/search"),
        Some("key-slow"),
        &json!({ "query": "slow query" }),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        elapsed >= Duration::from_millis(250),
        "key-slow delayed {elapsed:?}, expected at least 250ms"
    );
    matrix.record(
        "mock_keenable",
        "key-slow search",
        200,
        format!("delayed_ms>={}", elapsed.as_millis()),
    );

    // MCP initialize: vendor server info, application/json.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-good"),
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        body["result"]["serverInfo"]["name"],
        json!("keenable-mcp-server")
    );
    matrix.record(
        "mock_keenable",
        "mcp initialize",
        200,
        format!("serverInfo.name={}", body["result"]["serverInfo"]["name"]),
    );

    // MCP tools/list: exactly the two canonical tools.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-good"),
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], json!("search_web_pages"));
    assert_eq!(tools[1]["name"], json!("fetch_page_content"));
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["url"]));
    matrix.record(
        "mock_keenable",
        "mcp tools/list",
        200,
        "tools=search_web_pages,fetch_page_content",
    );

    // MCP tools/call search: native payload + keenable/usage in _meta.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-good"),
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "search_web_pages", "arguments": { "query": "mcp query" } }
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(
        body["result"]["structuredContent"]["query"],
        json!("mcp query")
    );
    assert_eq!(
        body["result"]["_meta"]["keenable/usage"]["sku"],
        json!("search")
    );
    assert!(
        body["result"]["_meta"]["keenable/usage"]["credits_remaining"]
            .as_u64()
            .is_some()
    );
    matrix.record(
        "mock_keenable",
        "mcp tools/call search_web_pages",
        200,
        format!(
            "_meta.credits_remaining={}",
            body["result"]["_meta"]["keenable/usage"]["credits_remaining"]
        ),
    );

    // MCP tools/call fetch.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-good"),
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "fetch_page_content", "arguments": { "url": "https://example.com" } }
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(
        body["result"]["structuredContent"]["url"],
        json!("https://example.com")
    );
    assert_eq!(
        body["result"]["_meta"]["keenable/usage"]["sku"],
        json!("fetch")
    );
    matrix.record(
        "mock_keenable",
        "mcp tools/call fetch_page_content",
        200,
        format!(
            "_meta.sku={}",
            body["result"]["_meta"]["keenable/usage"]["sku"]
        ),
    );

    // MCP bad key: same 401 envelope as REST.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-auth"),
        &json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["code"], json!("invalid_api_key"));
    matrix.record(
        "mock_keenable",
        "mcp key-auth",
        401,
        format!("code={}", body["code"]),
    );

    // MCP unknown method: JSON-RPC method-not-found.
    let response = post_json(
        &client,
        &format!("{base}/mcp"),
        Some("key-good"),
        &json!({ "jsonrpc": "2.0", "id": 6, "method": "unknown/method" }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["error"]["code"], json!(-32601));
    matrix.record(
        "mock_keenable",
        "mcp unknown method",
        200,
        format!("jsonrpc_error={}", body["error"]["code"]),
    );

    // Observation endpoints: exact counters and recorded bodies.
    let metrics: Value = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request")
        .json()
        .await
        .expect("metrics json");
    assert_eq!(metrics["search"], json!(9));
    assert_eq!(metrics["fetch"], json!(1));
    assert_eq!(metrics["mcp"], json!(6));
    assert_eq!(metrics["mcp_initialize"], json!(1));
    assert_eq!(metrics["mcp_tools_list"], json!(1));
    assert_eq!(metrics["mcp_tools_call"], json!(2));
    assert_eq!(metrics["auth_failures"], json!(3));
    assert_eq!(metrics["slow_requests"], json!(1));
    assert_eq!(metrics["last_api_key"], json!("key-good"));

    let last: Value = client
        .get(format!("{base}/last-bodies"))
        .send()
        .await
        .expect("last-bodies request")
        .json()
        .await
        .expect("last-bodies json");
    assert_eq!(last["search"]["query"], json!("slow query"));
    assert!(
        last["fetch"]
            .as_str()
            .expect("fetch query string")
            .contains("url=")
    );
    assert_eq!(last["mcp"]["method"], json!("unknown/method"));
    matrix.record(
        "mock_keenable",
        "metrics + last-bodies",
        200,
        format!(
            "search={} mcp={} auth_failures={}",
            metrics["search"], metrics["mcp"], metrics["auth_failures"]
        ),
    );

    matrix.finish(17);
}

#[tokio::test]
async fn mock_zhihu_scripted_branches_and_observation_endpoints() {
    let (_mock, base) = spawn_mock(
        support_binaries::mock_zhihu_binary(),
        &["--bind", "127.0.0.1:0"],
    )
    .await;
    println!("mock_zhihu base={base}");
    let client = client();
    let mut matrix = Matrix::default();

    async fn zhihu_get(
        client: &reqwest::Client,
        url: &str,
        token: Option<&str>,
        timestamp: Option<&str>,
    ) -> reqwest::Response {
        let mut request = client.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(timestamp) = timestamp {
            request = request.header("x-request-timestamp", timestamp);
        }
        request.send().await.expect("request completes")
    }

    // tok-good zhihu_search: Code 0 with the native item shape.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI&Count=3"),
        Some("tok-good"),
        Some("1789000000"),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(0));
    assert_eq!(body["Message"], json!("success"));
    assert_eq!(body["Data"]["HasMore"], json!(false));
    assert!(
        body["Data"]["SearchHashId"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty())
    );
    assert_eq!(body["Data"]["Items"][0]["ContentType"], json!("Article"));
    assert!(
        body["Data"]["Items"][0]["Title"]
            .as_str()
            .is_some_and(|title| title.contains("AI"))
    );
    assert!(body["Data"]["Items"][0]["RankingScore"].is_number());
    assert_eq!(body["Data"]["Items"][0]["AuthorityLevel"], json!("4"));
    matrix.record(
        "mock_zhihu",
        "tok-good zhihu_search",
        200,
        format!(
            "Code=0 items={} title={}",
            body["Data"]["Items"].as_array().map_or(0, Vec::len),
            body["Data"]["Items"][0]["Title"]
        ),
    );

    // tok-good global_search.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/global_search?Query=AI&Count=3&SearchDB=all"),
        Some("tok-good"),
        Some("1789000000"),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(0));
    assert_eq!(
        body["Data"]["SearchHashId"],
        json!("mock-global_search-hash")
    );
    matrix.record(
        "mock_zhihu",
        "tok-good global_search",
        200,
        format!("Code=0 hash={}", body["Data"]["SearchHashId"]),
    );

    // tok-auth: 20001 authorization failure.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI"),
        Some("tok-auth"),
        Some("1789000000"),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(20001));
    assert_eq!(body["Message"], json!("Authorization failed"));
    assert_eq!(body["Data"], Value::Null);
    matrix.record(
        "mock_zhihu",
        "tok-auth zhihu_search",
        200,
        format!("Code={}", body["Code"]),
    );

    // tok-rate: 30001 rate limit.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI"),
        Some("tok-rate"),
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(30001));
    assert_eq!(body["Data"], Value::Null);
    matrix.record(
        "mock_zhihu",
        "tok-rate zhihu_search",
        200,
        format!("Code={}", body["Code"]),
    );

    // tok-internal: 90001 internal failure.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/global_search?Query=AI"),
        Some("tok-internal"),
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(90001));
    matrix.record(
        "mock_zhihu",
        "tok-internal global_search",
        200,
        format!("Code={}", body["Code"]),
    );

    // tok-badparam: 10001 parameter error.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI"),
        Some("tok-badparam"),
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(10001));
    matrix.record(
        "mock_zhihu",
        "tok-badparam zhihu_search",
        200,
        format!("Code={}", body["Code"]),
    );

    // QA failure case: no X-Request-Timestamp is an auth failure and the
    // counter increments from the tok-auth case.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI&Count=3"),
        Some("tok-good"),
        None,
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(20001));
    assert_eq!(body["Message"], json!("Authorization failed"));
    let metrics: Value = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request")
        .json()
        .await
        .expect("metrics json");
    assert_eq!(metrics["auth_failures"], json!(2));
    matrix.record(
        "mock_zhihu",
        "missing X-Request-Timestamp",
        200,
        format!("Code=20001 auth_failures={}", metrics["auth_failures"]),
    );

    // Missing Authorization is the same auth arm.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/content/zhihu_search?Query=AI&Count=3"),
        None,
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(20001));
    matrix.record(
        "mock_zhihu",
        "missing Authorization",
        200,
        format!("Code={}", body["Code"]),
    );

    // Quota for a healthy token: both entries with daily totals.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/quota?APIIDs=zhihu_search,global_search"),
        Some("tok-good"),
        Some("1789000000"),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(0));
    let entries = body["Data"].as_array().expect("quota entries");
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert_eq!(entry["TotalQuota"], json!(5000));
        assert!(entry["RemainingQuota"].as_u64().expect("remaining") > 0);
    }
    let remaining: Vec<u64> = entries
        .iter()
        .map(|entry| entry["RemainingQuota"].as_u64().expect("remaining"))
        .collect();
    matrix.record(
        "mock_zhihu",
        "tok-good quota",
        200,
        format!("Code=0 remaining={remaining:?}"),
    );

    // tok-quota-zero: both capabilities report zero remaining.
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/quota?APIIDs=zhihu_search,global_search"),
        Some("tok-quota-zero"),
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(0));
    for entry in body["Data"].as_array().expect("quota entries") {
        assert_eq!(entry["RemainingQuota"], json!(0));
        assert_eq!(entry["TotalUsed"], entry["TotalQuota"]);
    }
    matrix.record(
        "mock_zhihu",
        "tok-quota-zero quota",
        200,
        "Code=0 remaining=0,0",
    );

    // tok-quota-tight: the 3/10 QA plan (zhihu_search 3/10, global 5/20).
    let response = zhihu_get(
        &client,
        &format!("{base}/api/v1/quota?APIIDs=zhihu_search,global_search"),
        Some("tok-quota-tight"),
        Some("1789000000"),
    )
    .await;
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["Code"], json!(0));
    let entries = body["Data"].as_array().expect("quota entries");
    let zhihu_entry = entries
        .iter()
        .find(|entry| entry["APIID"] == json!("zhihu_search"))
        .expect("zhihu_search entry");
    assert_eq!(zhihu_entry["TotalQuota"], json!(10));
    assert_eq!(zhihu_entry["RemainingQuota"], json!(3));
    let global_entry = entries
        .iter()
        .find(|entry| entry["APIID"] == json!("global_search"))
        .expect("global_search entry");
    assert_eq!(global_entry["TotalQuota"], json!(20));
    assert_eq!(global_entry["RemainingQuota"], json!(5));
    matrix.record(
        "mock_zhihu",
        "tok-quota-tight quota",
        200,
        "Code=0 zhihu_search=3/10 global_search=5/20",
    );

    // Observation endpoints.
    let metrics: Value = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("metrics request")
        .json()
        .await
        .expect("metrics json");
    assert_eq!(metrics["zhihu_search"], json!(6));
    assert_eq!(metrics["global_search"], json!(2));
    assert_eq!(metrics["quota"], json!(3));
    assert_eq!(metrics["auth_failures"], json!(3));
    assert_eq!(
        metrics["last_authorization"],
        json!("Bearer tok-quota-tight")
    );
    assert_eq!(metrics["last_x_request_timestamp"], json!("1789000000"));

    let last: Value = client
        .get(format!("{base}/last-bodies"))
        .send()
        .await
        .expect("last-bodies request")
        .json()
        .await
        .expect("last-bodies json");
    assert_eq!(last["zhihu_search"], json!("Query=AI&Count=3"));
    assert_eq!(last["global_search"], json!("Query=AI"));
    assert!(
        last["quota"]
            .as_str()
            .expect("quota query string")
            .contains("APIIDs=")
    );
    matrix.record(
        "mock_zhihu",
        "metrics + last-bodies",
        200,
        format!(
            "zhihu_search={} global_search={} quota={} auth_failures={}",
            metrics["zhihu_search"],
            metrics["global_search"],
            metrics["quota"],
            metrics["auth_failures"]
        ),
    );

    matrix.finish(12);
}
