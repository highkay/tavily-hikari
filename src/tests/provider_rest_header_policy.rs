//! keenable-integration todo 4 QA battery: the provider REST executor's
//! outbound header policy, exercised end-to-end against a local recording
//! mock bound to a 127.0.0.1 loopback port only (the repo stub-only rule).
//!
//! The full executor loop for Keenable/Zhihu becomes reachable when todos 6/7
//! register their routes; until then this battery drives the exact seam
//! functions the executor calls, in the executor's order (sanitizer ->
//! x-project-id gate -> per-attempt key injection -> per-attempt decorator ->
//! timeout-wrapped send), and reads what the mock actually received — not just
//! a 200 status (the misleading-success-output guard).
//!
//! Building a `TavilyProxy` runs the full SQLite schema bootstrap (~40s), so
//! one test covers all fast phases and the real-budget timeout lives behind
//! `#[ignore]` for the QA capture run.

use super::*;
use crate::providers::ProviderId;
use axum::extract::State;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// One recorded upstream request: the exact header map (lowercased names) and
/// body the mock received — the `/last-bodies` contract of the repo stubs.
#[derive(Clone, Debug)]
struct RecordedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Clone, Default)]
struct RecordingMock {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl RecordingMock {
    async fn last(&self) -> RecordedRequest {
        self.requests
            .lock()
            .await
            .last()
            .expect("at least one recorded upstream request")
            .clone()
    }
}

/// Loopback-only mock: every request is recorded, `/slow/*` paths sleep the
/// configured delay before answering, and `/last-bodies` replays the recorded
/// requests as JSON (the QA evidence surface).
async fn spawn_recording_mock(delay: Duration) -> (std::net::SocketAddr, RecordingMock) {
    let state = RecordingMock::default();
    let app = axum::Router::new()
        .route("/last-bodies", axum::routing::get(last_bodies))
        .fallback(record)
        .with_state((state.clone(), delay));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("mock port");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    (addr, state)
}

async fn record(
    State((state, delay)): State<(RecordingMock, Duration)>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> &'static str {
    let _ = method;
    if uri.path().starts_with("/slow") {
        tokio::time::sleep(delay).await;
    }
    let recorded = RecordedRequest {
        path: uri.path().to_string(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: String::from_utf8_lossy(&body).to_string(),
    };
    state.requests.lock().await.push(recorded);
    "ok"
}

async fn last_bodies(State((state, _)): State<(RecordingMock, Duration)>) -> axum::Json<Value> {
    let requests = state.requests.lock().await;
    axum::Json(Value::Array(
        requests
            .iter()
            .map(|request| {
                json!({
                    "path": request.path,
                    "headers": request.headers,
                    "body": request.body,
                })
            })
            .collect(),
    ))
}

async fn seed_provider_key(proxy: &TavilyProxy, provider: ProviderId, secret: &str) -> String {
    proxy
        .key_store
        .sync_keys(provider, &[secret.to_string()])
        .await
        .expect("seed provider key");
    sqlx::query_scalar("SELECT id FROM api_keys WHERE api_key = ? LIMIT 1")
        .bind(secret)
        .fetch_one(&proxy.key_store.pool)
        .await
        .expect("seeded key id")
}

/// The evil client header set every phase sends: forgeries for every value
/// todo 4 must never relay (client credential, stale Zhihu signature, pooled
/// key slot, Tavily-only policy header) plus benign allowed headers.
fn evil_client_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("accept", HeaderValue::from_static("application/json"));
    headers.insert("accept-encoding", HeaderValue::from_static("gzip"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer th-client-token"),
    );
    headers.insert("x-request-timestamp", HeaderValue::from_static("1"));
    headers.insert("x-api-key", HeaderValue::from_static("evil"));
    headers.insert("x-project-id", HeaderValue::from_static("evil"));
    headers.insert("cookie", HeaderValue::from_static("session=1"));
    headers
}

#[tokio::test]
async fn zhihu_and_keenable_outbound_header_policy_end_to_end() {
    let db_path = temp_db_path("provider-rest-header-policy");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("create proxy");
    let zhihu_key = seed_provider_key(&proxy, ProviderId::Zhihu, "zhihu-pooled-secret").await;
    let keenable_key =
        seed_provider_key(&proxy, ProviderId::Keenable, "keenable-pooled-secret").await;
    let (addr, mock) = spawn_recording_mock(Duration::from_secs(3)).await;

    // --- QA happy: a Zhihu outbound carries a fresh server-side
    // X-Request-Timestamp and Content-Type: application/json. ---
    let client_headers = evil_client_headers();
    let sanitize = provider_rest_header_sanitizer(ProviderId::Zhihu).expect("zhihu allowlist");
    let mut base_headers = sanitize(&client_headers);
    let project_id = proxy
        .apply_upstream_project_id_header_for_provider(
            ProviderId::Zhihu,
            &mut base_headers,
            &client_headers,
            None,
        )
        .await
        .expect("project-id gate runs");
    assert_eq!(project_id, None, "zhihu never derives an x-project-id");

    let upstream_url =
        Url::parse(&format!("http://{addr}/api/v1/content/zhihu_search")).expect("upstream url");
    let (url, mut outbound) = provider_rest_attempt_target(
        ProviderId::Zhihu,
        providers::spec_for(ProviderId::Zhihu),
        &upstream_url,
        &base_headers,
        "zhihu-pooled-secret",
    )
    .expect("attempt target");
    decorate_provider_rest_attempt_headers(ProviderId::Zhihu, &mut outbound, &proxy.backend_time);
    let expected_ts = proxy.backend_time.now_ts();

    let (response, _lease) = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Zhihu,
            &zhihu_key,
            "zhihu_http_zhihu_search",
            provider_rest_request_timeout(ProviderId::Zhihu),
            |client| {
                let mut builder = client.get(url.clone());
                for (name, value) in outbound.headers.iter() {
                    builder = builder.header(name, value);
                }
                builder
            },
        )
        .await
        .expect("zhihu outbound succeeds");
    assert_eq!(response.status(), 200);
    let _ = response.text().await;

    let recorded = mock.last().await;
    assert_eq!(
        recorded.headers.get("authorization").map(String::as_str),
        Some("Bearer zhihu-pooled-secret"),
        "the pooled Access Secret is the only authorization upstream"
    );
    let sent_ts: i64 = recorded
        .headers
        .get("x-request-timestamp")
        .expect("server-side timestamp stamped")
        .parse()
        .expect("integer unix-second timestamp");
    assert_ne!(sent_ts, 1, "a client-supplied timestamp is never relayed");
    assert!(
        (sent_ts - expected_ts).abs() <= 2,
        "timestamp must be fresh from the backend clock, got {sent_ts} around {expected_ts}"
    );
    assert_eq!(
        recorded.headers.get("content-type").map(String::as_str),
        Some("application/json"),
        "zhihu outbound is stamped Content-Type: application/json"
    );
    for absent in ["x-api-key", "x-project-id", "cookie"] {
        assert!(
            !recorded.headers.contains_key(absent),
            "client value {absent} leaked into the zhihu outbound"
        );
    }
    // No client credential or forged value survives anywhere in the outbound
    // header set (the mock's own reqwest-added user-agent is not client input).
    for value in recorded.headers.values() {
        assert!(
            !value.contains("evil") && !value.contains("th-client-token"),
            "client value leaked into the zhihu outbound: {value}"
        );
    }

    // --- Keenable: pooled X-API-Key injection, no decorator, no timestamp. ---
    let client_headers = evil_client_headers();
    let sanitize =
        provider_rest_header_sanitizer(ProviderId::Keenable).expect("keenable allowlist");
    let mut base_headers = sanitize(&client_headers);
    proxy
        .apply_upstream_project_id_header_for_provider(
            ProviderId::Keenable,
            &mut base_headers,
            &client_headers,
            None,
        )
        .await
        .expect("project-id gate runs");
    let upstream_url = Url::parse(&format!("http://{addr}/v1/search")).expect("upstream url");
    let (url, mut outbound) = provider_rest_attempt_target(
        ProviderId::Keenable,
        providers::spec_for(ProviderId::Keenable),
        &upstream_url,
        &base_headers,
        "keenable-pooled-secret",
    )
    .expect("attempt target");
    decorate_provider_rest_attempt_headers(
        ProviderId::Keenable,
        &mut outbound,
        &proxy.backend_time,
    );

    let (response, _lease) = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Keenable,
            &keenable_key,
            "keenable_http_search",
            provider_rest_request_timeout(ProviderId::Keenable),
            |client| {
                let mut builder = client.post(url.clone()).body(r#"{"query":"ai"}"#);
                for (name, value) in outbound.headers.iter() {
                    builder = builder.header(name, value);
                }
                builder
            },
        )
        .await
        .expect("keenable outbound succeeds");
    assert_eq!(response.status(), 200);
    let _ = response.text().await;

    let recorded = mock.last().await;
    assert_eq!(
        recorded.headers.get("x-api-key").map(String::as_str),
        Some("keenable-pooled-secret"),
        "the pooled key rides the native X-API-Key slot"
    );
    assert!(
        !recorded.headers.contains_key("x-request-timestamp"),
        "keenable never carries a zhihu-style timestamp"
    );
    assert!(
        !recorded.headers.contains_key("authorization"),
        "keenable outbound carries no authorization"
    );
    assert_eq!(recorded.body, r#"{"query":"ai"}"#);
    for absent in ["x-project-id", "cookie"] {
        assert!(
            !recorded.headers.contains_key(absent),
            "client value {absent} leaked into the keenable outbound"
        );
    }
    for value in recorded.headers.values() {
        assert!(
            !value.contains("evil") && !value.contains("th-client-token"),
            "client value leaked into the keenable outbound: {value}"
        );
    }

    // --- Slow upstream: the timeout wrapper ends the attempt with the literal
    // message, and the egress slot is released (an immediate follow-up send
    // through the same seam completes). Small budget so the suite stays fast;
    // the real 20s budget is pinned by the #[ignore] QA test below. ---
    let slow_url = Url::parse(&format!("http://{addr}/slow/zhihu_search")).expect("slow url");
    let started = std::time::Instant::now();
    let err = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Zhihu,
            &zhihu_key,
            "zhihu_http_zhihu_search",
            Some(Duration::from_millis(150)),
            |client| client.get(slow_url.clone()),
        )
        .await
        .expect_err("a hung upstream must be cut by the timeout wrapper");
    assert_eq!(
        err.to_string(),
        "other error: provider request timed out",
        "the timeout error is the literal message, never a reqwest URL"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the wrapper (150ms budget), not the mock (3s delay), ended the attempt"
    );

    let fast_url =
        Url::parse(&format!("http://{addr}/api/v1/content/global_search")).expect("fast url");
    let (response, _lease) = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Zhihu,
            &zhihu_key,
            "zhihu_http_global_search",
            provider_rest_request_timeout(ProviderId::Zhihu),
            |client| client.get(fast_url.clone()),
        )
        .await
        .expect("egress slot released after the timed-out attempt");
    assert_eq!(response.status(), 200);
    let _ = response.text().await;
}

/// QA failure capture at the REAL documented budget: a Zhihu upstream slower
/// than the 20s total budget is cut at ~20s (long before the mock's 25s
/// delay), the error is the literal timeout message, and the seam is free for
/// the next attempt. `#[ignore]`d because it costs the proxy bootstrap plus a
/// wall-clock 20s wait.
#[tokio::test]
#[ignore = "runs the real 20s Zhihu timeout budget; QA capture only"]
async fn zhihu_real_budget_timeout_maps_to_server_transient_and_releases_egress() {
    let db_path = temp_db_path("zhihu-real-budget-timeout");
    let db_str = db_path.to_string_lossy().to_string();
    let proxy = TavilyProxy::with_endpoint(Vec::<String>::new(), DEFAULT_UPSTREAM, &db_str)
        .await
        .expect("create proxy");
    let zhihu_key = seed_provider_key(&proxy, ProviderId::Zhihu, "zhihu-pooled-secret").await;
    let (addr, _mock) = spawn_recording_mock(Duration::from_secs(25)).await;

    let slow_url = Url::parse(&format!("http://{addr}/slow/zhihu_search")).expect("slow url");
    let started = std::time::Instant::now();
    let err = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Zhihu,
            &zhihu_key,
            "zhihu_http_zhihu_search",
            provider_rest_request_timeout(ProviderId::Zhihu),
            |client| client.get(slow_url.clone()),
        )
        .await
        .expect_err("a hung upstream must be cut at the 20s budget");
    assert_eq!(err.to_string(), "other error: provider request timed out");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(19) && elapsed <= Duration::from_secs(23),
        "the wrapper cut the attempt at the 20s budget, not the mock at 25s: {elapsed:?}"
    );

    let fast_url =
        Url::parse(&format!("http://{addr}/api/v1/content/zhihu_search")).expect("fast url");
    let (response, _lease) = proxy
        .send_provider_rest_attempt_with_timeout(
            ProviderId::Zhihu,
            &zhihu_key,
            "zhihu_http_zhihu_search",
            provider_rest_request_timeout(ProviderId::Zhihu),
            |client| client.get(fast_url.clone()),
        )
        .await
        .expect("egress slot released after the timed-out attempt");
    assert_eq!(response.status(), 200);
    let _ = response.text().await;
}
