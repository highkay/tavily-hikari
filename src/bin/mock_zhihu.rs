//! Mock Zhihu open-platform upstream (content search + quota) for offline tests.
//!
//! The stub mirrors the verified vendor surface: both `GET
//! /api/v1/content/*` endpoints and the free `GET /api/v1/quota`, requiring
//! `Authorization: Bearer` plus `X-Request-Timestamp`, always answering HTTP
//! 200 with the `{Code,Message,Data}` envelope. Scripted tokens drive the
//! classifier arms the facade consumes; `tok-quota-zero` reports a spent
//! natural-day quota and `tok-quota-tight` reports the 3/10 QA plan.
//!
//! `GET /metrics` and `GET /last-bodies` are additive observation endpoints:
//! facade suites use them to prove request counts, header forwarding, and that
//! no query is silently rewritten.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};

const TOK_GOOD: &str = "tok-good";
const TOK_AUTH: &str = "tok-auth";
const TOK_RATE: &str = "tok-rate";
const TOK_INTERNAL: &str = "tok-internal";
const TOK_BADPARAM: &str = "tok-badparam";
const TOK_QUOTA_ZERO: &str = "tok-quota-zero";
/// Tight natural-day plan used by the quota-sync QA: `zhihu_search` 3/10 and
/// `global_search` 5/20, so the adapter must store remaining=3, limit=10.
const TOK_QUOTA_TIGHT: &str = "tok-quota-tight";

const QUOTA_TOTAL: u64 = 5_000;

#[derive(Parser)]
#[command(
    name = "mock_zhihu",
    about = "Mock Zhihu open-platform upstream for offline tests"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:58095")]
    bind: String,
    #[arg(
        long,
        env = "MOCK_ZHIHU_TOKENS",
        value_delimiter = ',',
        default_value = "mock-zhihu-token,tok-good"
    )]
    tokens: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum TokenStatus {
    Good,
    Auth,
    Rate,
    Internal,
    BadParam,
    QuotaZero,
    QuotaTight,
}

#[derive(Clone, Copy)]
enum Gate {
    Good,
    QuotaZero,
    QuotaTight,
}

#[derive(Debug, Default)]
struct MockCounters {
    zhihu_search: AtomicU64,
    global_search: AtomicU64,
    quota: AtomicU64,
    auth_failures: AtomicU64,
}

#[derive(Debug, Default)]
struct LastState {
    authorization: Mutex<Option<String>>,
    timestamp: Mutex<Option<String>>,
    zhihu_search: Mutex<Option<String>>,
    global_search: Mutex<Option<String>>,
    quota: Mutex<Option<String>>,
}

struct MockState {
    tokens: Vec<String>,
    counters: MockCounters,
    last: LastState,
}

impl MockState {
    fn classify(&self, token: &str) -> TokenStatus {
        match token {
            TOK_GOOD => TokenStatus::Good,
            TOK_AUTH => TokenStatus::Auth,
            TOK_RATE => TokenStatus::Rate,
            TOK_INTERNAL => TokenStatus::Internal,
            TOK_BADPARAM => TokenStatus::BadParam,
            TOK_QUOTA_ZERO => TokenStatus::QuotaZero,
            TOK_QUOTA_TIGHT => TokenStatus::QuotaTight,
            _ if self.tokens.iter().any(|known| known == token) => TokenStatus::Good,
            _ => TokenStatus::Auth,
        }
    }
}

fn remember(slot: &Mutex<Option<String>>, value: Option<&str>) {
    *slot.lock().expect("last query lock") = Some(value.unwrap_or_default().to_string());
}

fn read_query(slot: &Mutex<Option<String>>) -> Value {
    slot.lock()
        .expect("last query lock")
        .clone()
        .map_or(Value::Null, Value::String)
}

fn envelope(code: i64, message: &str, data: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "Code": code, "Message": message, "Data": data })),
    )
        .into_response()
}

fn auth_failed(state: &MockState) -> Response {
    state.counters.auth_failures.fetch_add(1, Ordering::Relaxed);
    envelope(20001, "Authorization failed", Value::Null)
}

fn remember_headers(state: &MockState, headers: &HeaderMap) {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        *state.last.authorization.lock().expect("auth lock") = Some(value.to_string());
    }
    if let Some(value) = headers
        .get("x-request-timestamp")
        .and_then(|value| value.to_str().ok())
    {
        *state.last.timestamp.lock().expect("timestamp lock") = Some(value.to_string());
    }
}

fn authorize(state: &MockState, headers: &HeaderMap) -> Result<Gate, Box<Response>> {
    remember_headers(state, headers);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.trim().is_empty());
    let timestamp = headers
        .get("x-request-timestamp")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty());
    let (Some(token), Some(_)) = (bearer, timestamp) else {
        return Err(Box::new(auth_failed(state)));
    };
    match state.classify(token) {
        TokenStatus::Good => Ok(Gate::Good),
        TokenStatus::QuotaZero => Ok(Gate::QuotaZero),
        TokenStatus::QuotaTight => Ok(Gate::QuotaTight),
        TokenStatus::Auth => Err(Box::new(auth_failed(state))),
        TokenStatus::Rate => Err(Box::new(envelope(
            30001,
            "请求频率超过限制，请稍后重试",
            Value::Null,
        ))),
        TokenStatus::Internal => Err(Box::new(envelope(90001, "额度数据读取失败", Value::Null))),
        TokenStatus::BadParam => Err(Box::new(envelope(
            10001,
            "APIIDs参数格式错误或包含未知API ID",
            Value::Null,
        ))),
    }
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(rename = "Query")]
    query: Option<String>,
    // The remaining documented params pass through untouched; their values are
    // observable through GET /last-bodies, so only `Query` is read here.
    #[allow(dead_code)]
    #[serde(rename = "Count")]
    count: Option<u32>,
    #[allow(dead_code)]
    #[serde(rename = "SortBy")]
    sort_by: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "SearchDB")]
    search_db: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "Filter")]
    filter: Option<String>,
}

fn search_item(query: &str, endpoint: &str) -> Value {
    json!({
        "Title": format!("Mock Zhihu result for {query}"),
        "Headline": format!("mock {endpoint} headline"),
        "ContentType": "Article",
        "ContentID": "1234567890",
        "AuthorSignature": "mock-author-signature",
        "ContentText": format!("mock content for {query}"),
        "Url": "https://zhuanlan.zhihu.com/p/mock",
        "CommentCount": 0,
        "VoteUpCount": 0,
        "AuthorName": "Mock Author",
        "AuthorAvatar": "https://example.invalid/avatar.jpg",
        "AuthorBadge": "",
        "AuthorBadgeText": "",
        "EditTime": 1_788_278_526_u64,
        "CreateTime": 1_788_278_525_u64,
        "AuthorityLevel": "4",
        "RankingScore": 1.25_f64
    })
}

fn search_payload(query: &str, endpoint: &str) -> Value {
    json!({
        "HasMore": false,
        "SearchHashId": format!("mock-{endpoint}-hash"),
        "Items": [search_item(query, endpoint)]
    })
}

fn search_response(params: &SearchParams, endpoint: &str) -> Response {
    let Some(query) = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
    else {
        return envelope(10001, "Query cannot be empty", Value::Null);
    };
    envelope(0, "success", search_payload(query, endpoint))
}

async fn handle_zhihu_search(
    State(state): State<Arc<MockState>>,
    Query(params): Query<SearchParams>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Response {
    state.counters.zhihu_search.fetch_add(1, Ordering::Relaxed);
    remember(&state.last.zhihu_search, raw_query.as_deref());
    if let Err(response) = authorize(&state, &headers) {
        return *response;
    }
    search_response(&params, "zhihu_search")
}

async fn handle_global_search(
    State(state): State<Arc<MockState>>,
    Query(params): Query<SearchParams>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Response {
    state.counters.global_search.fetch_add(1, Ordering::Relaxed);
    remember(&state.last.global_search, raw_query.as_deref());
    if let Err(response) = authorize(&state, &headers) {
        return *response;
    }
    search_response(&params, "global_search")
}

#[derive(Deserialize)]
struct QuotaParams {
    #[serde(rename = "APIIDs")]
    api_ids: Option<String>,
}

fn quota_entry(api_id: &str, api_name: &str, total: u64, used: u64) -> Value {
    json!({
        "APIID": api_id,
        "APIName": api_name,
        "TotalQuota": total,
        "TotalUsed": used,
        "RemainingQuota": total - used
    })
}

fn quota_payload(gate: Gate, requested: Option<&[String]>) -> Value {
    let all = match gate {
        Gate::Good => [
            quota_entry("global_search", "全网搜索", QUOTA_TOTAL, 1),
            quota_entry("zhihu_search", "知乎搜索", QUOTA_TOTAL, 2),
        ],
        Gate::QuotaZero => [
            quota_entry("global_search", "全网搜索", QUOTA_TOTAL, QUOTA_TOTAL),
            quota_entry("zhihu_search", "知乎搜索", QUOTA_TOTAL, QUOTA_TOTAL),
        ],
        Gate::QuotaTight => [
            quota_entry("global_search", "全网搜索", 20, 15),
            quota_entry("zhihu_search", "知乎搜索", 10, 7),
        ],
    };
    let entries: Vec<Value> = all
        .into_iter()
        .filter(|entry| match requested {
            None => true,
            Some(ids) => entry
                .get("APIID")
                .and_then(Value::as_str)
                .is_some_and(|id| ids.iter().any(|requested| requested == id)),
        })
        .collect();
    Value::Array(entries)
}

async fn handle_quota(
    State(state): State<Arc<MockState>>,
    Query(params): Query<QuotaParams>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> Response {
    state.counters.quota.fetch_add(1, Ordering::Relaxed);
    remember(&state.last.quota, raw_query.as_deref());
    let gate = match authorize(&state, &headers) {
        Ok(gate) => gate,
        Err(response) => return *response,
    };
    let requested: Option<Vec<String>> = params.api_ids.as_deref().map(|ids| {
        ids.split(',')
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect()
    });
    if let Some(ids) = requested.as_deref()
        && ids
            .iter()
            .any(|id| id != "zhihu_search" && id != "global_search")
    {
        return envelope(10001, "APIIDs参数格式错误或包含未知API ID", Value::Null);
    }
    envelope(0, "success", quota_payload(gate, requested.as_deref()))
}

async fn handle_metrics(State(state): State<Arc<MockState>>) -> Json<Value> {
    Json(json!({
        "zhihu_search": state.counters.zhihu_search.load(Ordering::Relaxed),
        "global_search": state.counters.global_search.load(Ordering::Relaxed),
        "quota": state.counters.quota.load(Ordering::Relaxed),
        "auth_failures": state.counters.auth_failures.load(Ordering::Relaxed),
        "last_authorization": state
            .last
            .authorization
            .lock()
            .expect("auth lock")
            .clone(),
        "last_x_request_timestamp": state
            .last
            .timestamp
            .lock()
            .expect("timestamp lock")
            .clone()
    }))
}

async fn handle_last_bodies(State(state): State<Arc<MockState>>) -> Json<Value> {
    Json(json!({
        "zhihu_search": read_query(&state.last.zhihu_search),
        "global_search": read_query(&state.last.global_search),
        "quota": read_query(&state.last.quota)
    }))
}

fn build_router(state: Arc<MockState>) -> Router {
    Router::new()
        .route("/api/v1/content/zhihu_search", get(handle_zhihu_search))
        .route("/api/v1/content/global_search", get(handle_global_search))
        .route("/api/v1/quota", get(handle_quota))
        .route("/metrics", get(handle_metrics))
        .route("/last-bodies", get(handle_last_bodies))
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let state = Arc::new(MockState {
        tokens: cli.tokens,
        counters: MockCounters::default(),
        last: LastState::default(),
    });
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&cli.bind).await?;
    println!(
        "Mock Zhihu upstream listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;
    Ok(())
}
