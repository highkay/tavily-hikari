# Zhihu Provider

Zhihu's open platform provides in-site content search (`zhihu_search`) and web-wide search
(`global_search`) with an HTTP-200 `Code` envelope. Hikari fronts both as native GET facades and
serves a local MCP facade, billed from the shared wallet.

## At a glance

| Item           | Value                                                                            |
| -------------- | -------------------------------------------------------------------------------- |
| REST base      | `https://<your-host>/api/zhihu` (upstream default `https://developer.zhihu.com`) |
| REST auth slot | `Authorization: Bearer th-...` (no native client slot)                           |
| MCP endpoint   | `https://<your-host>/zhihu/mcp` (local facade)                                   |
| MCP tools      | `zhihu_search`, `zhihu_global_search`                                            |
| Credits        | 1 per `Code == 0` response                                                       |
| Quota          | active daily sync from the free `/api/v1/quota`                                  |
| Recovery       | `RemainingQuota = 0` exhausts until the next natural day                         |

## REST endpoints

`GET /api/zhihu/api/v1/content/zhihu_search` takes `Query` (required, non-empty), `Count`
(default 10, max 10), and `SortBy` (`field:direction:(min,max)`). A `Count` above 10 returns a
local 400 with zero upstream calls; `Count <= 0` passes through so the upstream default
applies.

`GET /api/zhihu/api/v1/content/global_search` takes `Query` (required), `Count` (default 10,
max 20, same local guard), `SearchDB` (`all` default, `realtime`, `static`), and `Filter`
(uppercase `AND`/`OR`, `AND` binds tighter, parentheses group).

Both answer with the native `{Code, Message, Data}` envelope: `0` success, `10001` parameter
error, `20001` auth failure, `30001` rate limit, `90001` internal error, unknown non-zero =
client error. Only `Code == 0` bills 1 credit.

Upstream auth uses the pooled Access Secret in `Authorization: Bearer` plus a server-generated
`X-Request-Timestamp`; client-supplied auth headers and timestamps are never forwarded.

## MCP

`/zhihu/mcp` is a local facade. The provider's official MCP is SSE-only and is not used.
Responses are SSE-wrapped; a GET that opens an event stream gets 405, unknown tools get 400.

- `zhihu_search`: `query` required; `count` 1..10; `sort_by` optional
- `zhihu_global_search`: `query` required; `count` 1..20; `search_db` all/realtime/static;
  `filter` optional
- `initialize`, `ping`, `tools/list`, `notifications` are free; `tools/call` bills 1 credit
  only when the translated REST result is `Code == 0`, otherwise it returns an MCP tool error
  and bills 0

## Limits and quota semantics

- Count caps: 10 for in-site search, 20 for global search
- daily quota comes from the free `/api/v1/quota` call; Hikari reads both APIs at once, stores
  the lower `RemainingQuota`, and records the next natural-day (Asia/Shanghai midnight)
  renewal time
- `RemainingQuota = 0` marks the key exhausted until that natural-day boundary
- a missing quota entry is a typed sync error, never a silent 0

## Zero-cost validation

Admin key validation calls `/api/v1/quota`: `Code = 0` means valid, `20001` invalid. The quota
call consumes no business quota. Billable validation probes are forbidden.

## Environment

| Env var          | Default                       | Meaning                                  |
| ---------------- | ----------------------------- | ---------------------------------------- |
| `ZHIHU_API_KEYS` | unset                         | comma-separated bootstrap Access Secrets |
| `ZHIHU_UPSTREAM` | `https://developer.zhihu.com` | REST upstream base URL                   |

The CLI equivalents are `--zhihu-keys` and `--zhihu-upstream`. There is no separate MCP
upstream variable because `/zhihu/mcp` is served locally.

## Operator terms note

The Zhihu Access Secret is account-level. Hikari offers Zhihu to end users only through the
shared wallet and never automates multi-account rotation or free-tier farming. Before any
public offering, verify that reselling through your plan is allowed by the Zhihu open
platform's terms.

## Operating notes

- Deployment egress: verify `developer.zhihu.com` reachability from the target network at
  deployment time (transport level). This is an operator runbook step, never a CI job.
- Testing discipline: tests and dev scripts use the Hikari stub upstream (`mock_zhihu`) only;
  never point them at the production endpoint.
