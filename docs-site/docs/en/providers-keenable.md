# Keenable Provider

Keenable is a web search and page fetch API with a hosted MCP server. Through Hikari, the same
`th-` token reaches it over native REST facades plus a hosted MCP passthrough, billed from the
shared wallet.

## At a glance

| Item            | Value                                                                           |
| --------------- | ------------------------------------------------------------------------------- |
| REST base       | `https://<your-host>/api/keenable` (upstream default `https://api.keenable.ai`) |
| REST auth slots | `X-API-Key: th-...` or `Authorization: Bearer th-...`                           |
| MCP endpoint    | `https://<your-host>/keenable/mcp` (hosted passthrough)                         |
| MCP tools       | `search_web_pages`, `fetch_page_content`                                        |
| Credits         | 1 per successful search, fetch, or MCP `tools/call`                             |
| Quota           | passive harvest of `usage.credits_remaining`                                    |
| Recovery        | `402` exhausts until the next UTC month or a manual restore                     |

## REST endpoints

`POST /api/keenable/v1/search` passes the JSON body through unchanged. Notable parameters:
`query` (required), `mode` (`pro` default, `realtime`), `max_results` 1..50 (default 10),
`site`, `exclude_domains`, `published_after` and `published_before`, `acquired_after` and
`acquired_before`, `query_time`, `snippet_max_length` 180..10000, `safety_mode`,
`disable_cards`, `session_id`.

`GET /api/keenable/v1/fetch` passes the query string through unchanged. Notable parameters:
`url` (required), `max_chars` (default 50000), `live`, `prompt` (max 2000 chars), `session_id`.

Response bodies pass through unchanged after credential redaction; `usage` appears on keyed
success.

## MCP

`/keenable/mcp` is a buffered passthrough to the provider-hosted MCP server (Streamable HTTP,
JSON-RPC 2.0, no SSE). Auth: `X-API-Key: th-...` or bearer.

- `initialize`, `ping`, `notifications/*`, `tools/list`: free
- `tools/call` with `search_web_pages` or `fetch_page_content`: 1 credit on success
- successful tool calls carry `_meta["keenable/usage"]` with `credits_remaining`; Hikari
  harvests it

`search_web_pages` takes `query`, `site`, `exclude_domains`, date filters, `max_results` 1..50,
`snippet_max_length`, `mode`, `output_format`, `session_id`, `disable_cards`.
`fetch_page_content` takes `url`, `max_chars`, `live`, `prompt`, `session_id`, `output_format`.

## Limits and quota semantics

- 10 requests per second per organization, shared by all its keys and by search plus fetch
- 100,000 free requests per month on the provider's published free tier at contract time,
  then paid packages at $4 per 1,000 requests as published at contract time; plans change, so
  re-check with the provider before relying on either figure
- `usage.credits_remaining` is an estimate that can lag; Hikari samples it from every keyed
  REST success and MCP `tools/call` and persists it on material change only
- admin `sync-usage` returns the last passive snapshot; Keenable has no active poll
- `402` exhausts the key until the next UTC month boundary or a manual restore; `401`
  quarantines; `429` follows `Retry-After`; `target_error` is a client error and never marks
  a key

## Zero-cost validation

Admin key validation sends a search with an empty body (missing `query`): a `400 missing_parameter` means the key is valid, `401` means invalid, and any other status is a typed
reason. The probe never charges the account. Billable validation probes are forbidden.

## Environment

| Env var                 | Default                       | Meaning                        |
| ----------------------- | ----------------------------- | ------------------------------ |
| `KEENABLE_API_KEYS`     | unset                         | comma-separated bootstrap keys |
| `KEENABLE_UPSTREAM`     | `https://api.keenable.ai`     | REST upstream base URL         |
| `KEENABLE_MCP_UPSTREAM` | `https://api.keenable.ai/mcp` | full hosted MCP endpoint       |

The CLI equivalents are `--keenable-keys`, `--keenable-upstream`, and
`--keenable-mcp-upstream`.

## Operating notes

- Deployment egress: before enabling Keenable keys, verify from the target network that
  `api.keenable.ai` is reachable at the transport level. This is an operator runbook step,
  never a CI job.
- Testing discipline: tests and dev scripts use the Hikari stub upstream (`mock_keenable`)
  only; never point them at the production endpoint.
