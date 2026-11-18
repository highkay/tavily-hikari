//! Zhihu LOCAL MCP facade (`/zhihu/mcp`, keenable-integration plan todo 10).
//!
//! Zhihu's official MCP is SSE-only (it holds an event-stream connection open),
//! so this surface is a Hikari-local facade built on the provider-parameterized
//! [`LocalMcpFacade`] framework (local_mcp_facade.rs): initialize/tools-list/
//! ping/notifications/prompts/resources are answered locally (session-less,
//! SSE-wrapped), and `tools/call` translates into a pooled-key Zhihu REST GET
//! executed by the lib seam `TavilyProxy::proxy_provider_mcp_zhihu_facade_call`
//! (Bearer Access Secret + fresh server-side `X-Request-Timestamp` per attempt,
//! todo 4 decorator; never a client value).
//!
//! Catalog provenance: the two tool names, parameters, and inputSchema mirror
//! the keenable-zhihu-providers SPEC tool catalog exactly (`zhihu_search`,
//! `zhihu_global_search`); no other Zhihu tool is exposed (hot_list,
//! question_discovery, zhida_openai, knowledge, user-data, creator, and tool
//! APIs are out of scope). Local validation is the typed catalog guard: the
//! required `query`, `count` bounded 1..10 (zhihu_search) / 1..20
//! (zhihu_global_search), and the `search_db` enum — rejected before any
//! upstream call. `sort_by`/`filter` pass through unvalidated (the upstream
//! owns that grammar).
//!
//! Billing (zhihu_search=1, global_search=1) resolves through the committed
//! todo-2 ProviderSpec credits table and the SAME reserve→settle path as the
//! brave/exa local facades; the credit settles ONLY when the translated REST
//! result's envelope carries `Code == 0` (the framework's provider seam owns
//! that predicate — a non-zero `Code` surfaces as an MCP tool error at zero
//! credits). The surface is single-attempt: no key recovery from the MCP
//! layer. The pooled Access Secret never appears in a tool result or log.

use super::local_mcp_facade::{
    LocalMcpFacade, LocalMcpToolDefinition, LocalMcpUpstreamRequest, handle_local_mcp_request_body,
};
use super::provider_mcp_serpapi::ProviderMcpFacadeContext;
use super::*;

/// serverInfo.name for the Zhihu facade (M19 parameterized identity).
const ZHIHU_LOCAL_MCP_SERVER_NAME: &str = "zhihu-search-hikari";

/// Upper bound of the tool's `count` argument (SPEC tool catalog: 10 for
/// `zhihu_search`, 20 for `zhihu_global_search`).
fn zhihu_tool_count_cap(advertised_name: &str) -> i64 {
    match advertised_name {
        "zhihu_search" => 10,
        "zhihu_global_search" => 20,
        other => unreachable!("catalog guarantees the advertised name, got {other:?}"),
    }
}

/// Typed catalog guard (the SPEC tool catalog constraints, enforced BEFORE any
/// upstream call): the arguments must be an object carrying a non-empty string
/// `query`; `count`, when present, must be an integer within 1..cap; and
/// `search_db`, when present on `zhihu_global_search`, must be one of
/// `all`/`realtime`/`static`. `sort_by`/`filter` and every other parameter pass
/// through unvalidated (the Zhihu upstream owns their grammar).
fn zhihu_validate_arguments(
    tool: &LocalMcpToolDefinition,
    arguments: Option<&Value>,
) -> Result<Value, String> {
    let expected = format!(
        "Invalid arguments for {}: expected object with required 'query' field",
        tool.advertised_name
    );
    let Some(Value::Object(map)) = arguments else {
        return Err(expected);
    };
    match map.get("query") {
        Some(value)
            if value.is_string() && !value.as_str().unwrap_or_default().trim().is_empty() => {}
        Some(_) => {
            return Err(format!(
                "Invalid arguments for {}: 'query' must be a non-empty string",
                tool.advertised_name
            ));
        }
        None => {
            return Err(format!(
                "Invalid arguments for {}: missing required 'query' field",
                tool.advertised_name
            ));
        }
    }
    if let Some(count) = map.get("count") {
        let Some(parsed) = count.as_i64() else {
            return Err(format!(
                "Invalid arguments for {}: 'count' must be an integer",
                tool.advertised_name
            ));
        };
        let cap = zhihu_tool_count_cap(tool.advertised_name);
        if !(1..=cap).contains(&parsed) {
            return Err(format!(
                "Invalid arguments for {}: 'count' must be an integer between 1 and {cap}",
                tool.advertised_name
            ));
        }
    }
    if tool.advertised_name == "zhihu_global_search"
        && let Some(search_db) = map.get("search_db")
        && !matches!(
            search_db.as_str(),
            Some("all") | Some("realtime") | Some("static")
        )
    {
        return Err(format!(
            "Invalid arguments for {}: 'search_db' must be one of all, realtime, static",
            tool.advertised_name
        ));
    }
    // The guards matched a present object, so returning the cloned arguments
    // object is exact.
    Ok(Value::Object(map.clone()))
}

/// Render one JSON scalar into a Zhihu REST query-parameter value.
fn zhihu_scalar_to_param(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(if *flag {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Translate validated tools/call arguments into the Zhihu REST query string
/// (M19: a NEW provider translation module — the Tavily translation is never
/// touched). The MCP argument names map onto the Zhihu REST parameter names
/// (`query`→`Query`, `count`→`Count`, `sort_by`→`SortBy`, `search_db`→`SearchDB`,
/// `filter`→`Filter`); every other parameter passes through under its own name
/// so the upstream owns its validation (the URL-encoding of `Filter` grammar
/// rides the form serializer).
fn zhihu_translate(
    tool: &LocalMcpToolDefinition,
    arguments: &Value,
) -> Result<LocalMcpUpstreamRequest, String> {
    let map = arguments.as_object().ok_or_else(|| {
        format!(
            "Invalid arguments for {}: expected an object",
            tool.advertised_name
        )
    })?;
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (key, value) in map {
        let rest_key = match key.as_str() {
            "query" => "Query",
            "count" => "Count",
            "sort_by" => "SortBy",
            "search_db" => "SearchDB",
            "filter" => "Filter",
            other => other,
        };
        match value {
            Value::Null => {}
            Value::Array(items) => {
                for item in items {
                    if let Some(text) = zhihu_scalar_to_param(item) {
                        serializer.append_pair(rest_key, &text);
                    }
                }
            }
            scalar => {
                if let Some(text) = zhihu_scalar_to_param(scalar) {
                    serializer.append_pair(rest_key, &text);
                }
            }
        }
    }
    let query = serializer.finish();
    Ok(LocalMcpUpstreamRequest::Get {
        query: (!query.is_empty()).then_some(query),
    })
}

// --- inputSchema builders (mirror the keenable-zhihu-providers SPEC catalog) ---

/// zhihu_search — Zhihu REST `GET /api/v1/content/zhihu_search` (in-site
/// content search).
fn zhihu_search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Non-empty search keyword (required)." },
            "count": { "type": "integer", "description": "Number of results to return.", "minimum": 1, "maximum": 10 },
            "sort_by": { "type": "string", "description": "Sort expression `field:direction:(min,max)` — fields CommentCount/VoteUpCount/EditTime/CreateTime, direction asc/desc/none, optional inclusive Unix-second bounds." }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// zhihu_global_search — Zhihu REST `GET /api/v1/content/global_search`
/// (web-wide search).
fn zhihu_global_search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Non-empty search keyword (required)." },
            "count": { "type": "integer", "description": "Number of results to return.", "minimum": 1, "maximum": 20 },
            "search_db": { "type": "string", "description": "Search database to query.", "enum": ["all", "realtime", "static"], "default": "all" },
            "filter": { "type": "string", "description": "Advanced filter grammar: host ==/!= quoted strings, publish_time ==/!=/>/>=/</<= Unix-second values, uppercase AND/OR with parentheses." }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// The Zhihu local-facade tool catalog (DATA). Credits resolve through the
/// ProviderSpec table via `credits_op` (zhihu_search=1, global_search=1);
/// `upstream_operation` labels reuse the ratified REST vocabulary — the MCP
/// call IS that REST operation (T16 precedent).
static ZHIHU_LOCAL_MCP_TOOLS: &[LocalMcpToolDefinition] = &[
    LocalMcpToolDefinition {
        advertised_name: "zhihu_search",
        description: "Zhihu content search — in-site search over Zhihu answers, articles, and posts",
        input_schema: zhihu_search_schema,
        validate: zhihu_validate_arguments,
        required_field: "query",
        credits_op: "zhihu_search",
        upstream_path: "/api/v1/content/zhihu_search",
        upstream_operation: "zhihu_http_zhihu_search",
    },
    LocalMcpToolDefinition {
        advertised_name: "zhihu_global_search",
        description: "Zhihu global search — web-wide search with realtime/static database selection",
        input_schema: zhihu_global_search_schema,
        validate: zhihu_validate_arguments,
        required_field: "query",
        credits_op: "global_search",
        upstream_path: "/api/v1/content/global_search",
        upstream_operation: "zhihu_http_global_search",
    },
];

/// The Zhihu local MCP facade descriptor (M19 provider-parameterized identity +
/// catalog DATA + translate hook), completing the todo-10 arm of the todo-16
/// framework's provider set.
static ZHIHU_LOCAL_MCP_FACADE: LocalMcpFacade = LocalMcpFacade {
    provider: ProviderId::Zhihu,
    server_name: ZHIHU_LOCAL_MCP_SERVER_NAME,
    ingress_path: "/zhihu/mcp",
    tools: ZHIHU_LOCAL_MCP_TOOLS,
    translate: zhihu_translate,
};

/// Zhihu local-facade body (replaces the typed todo-2 `provider_mcp_pending`
/// boundary for the zhihu surface only; Keenable keeps that boundary until
/// todo 8 lands its hosted passthrough).
pub(super) async fn provider_mcp_zhihu_local_facade(
    state: Arc<AppState>,
    ctx: ProviderMcpFacadeContext<'_>,
) -> Result<Response<Body>, StatusCode> {
    // Tavily Control-mode / serpapi-passthrough / brave/exa parity: a GET
    // opening an event stream is rejected locally — the facade buffers
    // responses, so a stream-open GET would hang instead of serving.
    if *ctx.method == Method::GET && accepts_event_stream(ctx.original_headers) {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::empty())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }
    // The facade is SESSION-LESS (never mints an mcp-session-id), so no session
    // id can ever resolve on this surface — including an ACTIVE Tavily control
    // session id presented on the shared token (the hosted-passthrough
    // `session_unavailable` vocabulary, 404).
    if header_string(&ctx.auth.headers, "mcp-session-id").is_some() {
        return mcp_session_response(
            StatusCode::NOT_FOUND,
            "session_unavailable",
            "MCP session is unavailable, please reconnect to initialize a new session.",
        );
    }
    let system_settings = state.proxy.get_system_settings().await.map_err(|err| {
        eprintln!("get system settings failed for {}: {err}", ctx.path);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let client_ip = resolve_client_ip_info(
        ctx.remote_addr,
        ctx.original_headers,
        &TrustedClientIpSettings {
            trusted_proxy_cidrs: system_settings.trusted_proxy_cidrs.clone(),
            trusted_client_ip_headers: system_settings.trusted_client_ip_headers.clone(),
        },
    );
    let incoming_protocol_version = header_string(&ctx.auth.headers, "mcp-protocol-version");

    handle_local_mcp_request_body(
        &ZHIHU_LOCAL_MCP_FACADE,
        &state,
        ctx.method,
        ctx.path,
        ctx.request_kind,
        ctx.body,
        &ctx.auth.headers,
        ctx.auth.token_id.as_deref(),
        ctx.auth.query.as_deref(),
        &client_ip,
        incoming_protocol_version.as_deref(),
    )
    .await
}
