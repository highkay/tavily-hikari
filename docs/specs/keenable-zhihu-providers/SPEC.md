# Keenable and Zhihu provider contracts

> Canonical contract for the keenable-integration implementation todos. Sources: live probes
> against both providers (2026-11-16), captured in
> `../../../.omo/evidence/keenable-integration/probes/`, plus the official Keenable docs
> (`api-reference.md`, `errors.md`, `credits.md`, `mcp-server.md`) and the Zhihu open-platform
> docs (search, global search, bearer auth, quota). Tests always hit stubs; a live capture never
> appears in CI.

## Scope

In scope: Keenable web search and page fetch with their hosted MCP passthrough, Zhihu in-site and
global search with a local MCP facade, provider-scoped key pools, per-provider failure
classification, passive (Keenable) and active daily (Zhihu) quota sync, `NaturalDay` recovery for
Zhihu, zero-cost admin validation probes, and a 1-credit billing table on the shared wallet.

Out of scope: Keenable's keyless `/public` twins, pay-per-request `/v1/x402/*`, CLI, and
Time Machine endpoints; Zhihu's `hot_list`, `question_discovery`, `zhida_openai`, knowledge,
user-data, creator, and tool APIs; any Tavily-compatible translation layer; cross-provider
fallback; streaming passthrough.

## Route table

| Hikari path                               | Provider | Method | Upstream path                   | Credits op      | Upstream operation         | Request-kind key          | Kind path                  |
| ----------------------------------------- | -------- | ------ | ------------------------------- | --------------- | -------------------------- | ------------------------- | -------------------------- |
| `/api/keenable/v1/search`                 | keenable | POST   | `/v1/search`                    | `search`        | `keenable_http_search`     | `api:keenable/search`     | `/api/keenable/search`     |
| `/api/keenable/v1/fetch`                  | keenable | GET    | `/v1/fetch`                     | `fetch`         | `keenable_http_fetch`      | `api:keenable/fetch`      | `/api/keenable/fetch`      |
| `/api/zhihu/api/v1/content/zhihu_search`  | zhihu    | GET    | `/api/v1/content/zhihu_search`  | `zhihu_search`  | `zhihu_http_zhihu_search`  | `api:zhihu/zhihu_search`  | `/api/zhihu/zhihu_search`  |
| `/api/zhihu/api/v1/content/global_search` | zhihu    | GET    | `/api/v1/content/global_search` | `global_search` | `zhihu_http_global_search` | `api:zhihu/global_search` | `/api/zhihu/global_search` |

MCP ingress:

| Hikari path     | Provider | Mode               | Upstream                        | Tools                                    | Request-kind keys                                                  |
| --------------- | -------- | ------------------ | ------------------------------- | ---------------------------------------- | ------------------------------------------------------------------ |
| `/keenable/mcp` | keenable | hosted passthrough | `https://api.keenable.ai/mcp`   | `search_web_pages`, `fetch_page_content` | `mcp:keenable/search_web_pages`, `mcp:keenable/fetch_page_content` |
| `/zhihu/mcp`    | zhihu    | local facade       | none (official MCP is SSE-only) | `zhihu_search`, `zhihu_global_search`    | `mcp:zhihu/zhihu_search`, `mcp:zhihu/global_search`                |

`/{provider}/mcp/*path` subpaths are rejected locally as `404 text/plain` and never reach the SPA
fallback or the upstream. A GET that tries to open an event stream on either MCP base gets `405`.

## Common ingress rules

- The client authenticates with the Hikari `th-` token in `Authorization: Bearer`. Keenable also
  accepts its client-native slot `X-API-Key`; Zhihu has no client-native header slot.
- Client-supplied `X-API-Key`, `Authorization`, and `X-Request-Timestamp` values are stripped before
  the upstream request and before logging, then the pooled credential is injected per provider.
- Upstream response bodies pass through to the client unchanged apart from credential redaction.

## Configuration surface

Env var names and defaults are owned by the config code (`src/main.rs`); this table is the
contract record and must stay in sync:

| Env var                 | Default                       | Meaning                                              |
| ----------------------- | ----------------------------- | ---------------------------------------------------- |
| `KEENABLE_API_KEYS`     | unset                         | Comma-separated Keenable bootstrap keys, scoped sync |
| `KEENABLE_UPSTREAM`     | `https://api.keenable.ai`     | Keenable REST upstream base URL (path prefix ok)     |
| `KEENABLE_MCP_UPSTREAM` | `https://api.keenable.ai/mcp` | Full Keenable hosted MCP endpoint                    |
| `ZHIHU_API_KEYS`        | unset                         | Comma-separated Zhihu bootstrap Access Secrets       |
| `ZHIHU_UPSTREAM`        | `https://developer.zhihu.com` | Zhihu REST upstream base URL (path prefix ok)        |

Bootstrap semantics: when one of the key lists is set, listed keys are added or revived and that
provider's unlisted keys are soft-deleted; when unset, that provider's key rows stay untouched.
There is no `ZHIHU_MCP_UPSTREAM`: `/zhihu/mcp` is a local facade. CLI flags mirror the env names
(`--keenable-upstream`, `--keenable-mcp-upstream`, `--zhihu-upstream`, `--keenable-keys`,
`--zhihu-keys`).

## Keenable REST

Base URL: `https://api.keenable.ai` (default; overridable via the upstream config).

### Search: `POST /v1/search`

Request: JSON body, `Content-Type: application/json`. Unknown parameters return `400`. Do not
translate any parameter.

| Parameter            | Type             | Required | Contract                                                                 |
| -------------------- | ---------------- | -------- | ------------------------------------------------------------------------ |
| `query`              | string           | yes      | Search query; operators `"..."`, `site:`, `OR`, `-`, `( )` allowed       |
| `mode`               | string           | no       | `pro` (default), `realtime`                                              |
| `site`               | string           | no       | Restrict to one site (subdomains included)                               |
| `exclude_domains`    | array of strings | no       | Host entries, clean-up applied, up to 1,000 distinct hosts               |
| `published_after`    | string           | no       | Date, ISO 8601 timestamp, or relative delta (`7d`, `30min`, `1mo`, `1y`) |
| `published_before`   | string           | no       | Same formats                                                             |
| `acquired_after`     | string           | no       | Same formats; index date                                                 |
| `acquired_before`    | string           | no       | Same formats; index date                                                 |
| `query_time`         | string           | no       | Point-in-time upper bound on index state                                 |
| `max_results`        | integer          | no       | `1..50`, default 10                                                      |
| `snippet_max_length` | integer          | no       | `180..10000`                                                             |
| `disable_cards`      | boolean          | no       | Default false                                                            |
| `safety_mode`        | string           | no       | `off` (default), `moderate`, `strict`                                    |
| `session_id`         | string           | no       | Up to 128 chars; `X-Keenable-Session-Id` header wins                     |

Response `200`:

| Field       | Contract                                                                                                                  |
| ----------- | ------------------------------------------------------------------------------------------------------------------------- |
| `query`     | Echo of the sent query                                                                                                    |
| `mode`      | Mode that served the request (`pro` or `realtime`)                                                                        |
| `results[]` | `title`, `url`, `snippet`, optional `published_at`/`acquired_at` (ISO 8601 UTC), `description` (deprecated, always empty) |
| `warning`   | Optional; present when the request was not a plain search                                                                 |
| `usage`     | On keyed success only: `sku`, `amount`, `credits`, `paid`, optional `credits_remaining`                                   |

### Fetch: `GET /v1/fetch`

Request: query parameters. Unknown parameters return `400`. Pass the query string through
unchanged.

| Parameter    | Type    | Required | Contract                                       |
| ------------ | ------- | -------- | ---------------------------------------------- |
| `url`        | string  | yes      | Absolute `http`/`https` URL                    |
| `max_chars`  | integer | no       | Positive; default 50000; hard cap on `content` |
| `live`       | boolean | no       | Default false; fetch from source now           |
| `prompt`     | string  | no       | Extraction instruction, up to 2000 chars       |
| `session_id` | string  | no       | Up to 128 chars; header wins                   |

Response `200`:

| Field          | Contract                                                                    |
| -------------- | --------------------------------------------------------------------------- |
| `url`          | Final address after redirects                                               |
| `content`      | Page as markdown, capped by `max_chars`; with `prompt`, the extraction only |
| `truncated`    | Optional; true when `content` was cut                                       |
| `total_chars`  | Optional; full page length before the cap                                   |
| `title`        | Optional; page title                                                        |
| `author`       | Optional                                                                    |
| `published_at` | Optional; ISO 8601 UTC                                                      |
| `description`  | Deprecated, always empty                                                    |
| `usage`        | On keyed success only, same shape as search                                 |

### Keenable error table

Every `/v1` error is one flat JSON object with `error`, `message`, and at least `type` and `code`;
a retryable error adds `retry_after_seconds` plus a `Retry-After` header. Branch on `type` and
`code`, never on `error` or `message` text. `type` is a closed set of seven values; the table fixes
the health mapping.

| `type`                    | Statuses                               | Example codes                                                                                                                                                                            | Hikari signal                                                                            |
| ------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `invalid_request_error`   | 400, 404, 405, 406, 409, 413, 415, 422 | `missing_parameter`, `invalid_request`, `unknown_parameter`                                                                                                                              | `ClientError`                                                                            |
| `authentication_error`    | 401                                    | `invalid_api_key`, `missing_api_key`, `api_key_required`                                                                                                                                 | `AuthFailed`                                                                             |
| `permission_error`        | 400, 403                               | `credentials_forbidden`, `forbidden`, `target_blocked`, `private_url_not_allowed`, `mode_requires_api_key`, `mcp_host_not_allowed`, `mcp_origin_not_allowed`, `time_machine_not_enabled` | `AuthFailed` for `credentials_forbidden`/`forbidden`; `ClientError` for every other code |
| `billing_error`           | 402, 451                               | `insufficient_credits`, `payment_required`                                                                                                                                               | `Exhausted` on `402` (all 402 codes mean the account cannot pay); `ClientError` on `451` |
| `rate_limit_error`        | 400, 429                               | `rate_limit_per_second`, `rate_limited`                                                                                                                                                  | `RateLimited { retry_after_secs }` from `retry_after_seconds` or `Retry-After`           |
| `target_error`            | 403, 404, 422, 429, 502, 504           | `target_blocked`, `content_not_extracted`, `target_rate_limited`, `target_timeout`                                                                                                       | `ClientError` regardless of status; never marks a key                                    |
| `api_error`               | 422, 5xx, 529                          | `search_failed_upstream`, `internal_error`, `search_overloaded`                                                                                                                          | `ServerTransient`                                                                        |
| missing or unknown `type` | any                                    | none                                                                                                                                                                                     | HTTP 2xx `Ok`; >= 500 `ServerTransient`; otherwise `ClientError`                         |

### Keenable rate limits, credits, and passive harvest

- Authenticated rate limit: 10 requests per second per organization, shared by all its keys and by
  search plus fetch. Responses carry `X-RateLimit-Limit`/`X-RateLimit-Remaining`/`X-RateLimit-Reset`
  (documented; the probe captures only show `content-type` and `x-request-id`), and a `429` adds
  `Retry-After`.
- Credits: 100,000 requests per month free, resetting monthly; after that, paid packages at $4 per
  1,000 requests. Search and fetch each cost one credit of their SKU.
- Passive harvest: `usage.credits_remaining` (unused monthly allowance plus purchased credits;
  estimate, may lag seconds, may be absent) is sampled from every keyed REST success and every MCP
  `tools/call` `_meta`, persisted only on a material change. `paid` says which balance paid.
- Passive rate-limit harvest (plan todo 22): when a keyed response carries all three of
  `X-RateLimit-Limit`/`-Remaining`/`-Reset`, Hikari snapshots the window in the existing `meta` KV
  row `keenable_rate_limit_v1:<api_key_id>` (no schema migration). `X-RateLimit-Reset` is stored
  verbatim (the vendor documents an ISO 8601 instant) and additionally parsed to epoch seconds in
  `reset_at`; a numeric stub hint stays raw. This per-second throughput signal is NEVER written to
  the monthly balance columns (`api_keys.quota_limit`/`quota_remaining`) or to
  `api_key_quota_sync_samples`, and a response without the triple writes nothing. The admin key
  detail exposes the last snapshot as `rate_limit`, rendered next to `credits_remaining` in the
  quota panel.

## Keenable hosted MCP passthrough

Endpoint: `POST https://api.keenable.ai/mcp`. Streamable HTTP (JSON-RPC 2.0), responses observed as
`application/json`; no SSE in the captures. Auth: pooled key in `X-API-Key`. `initialize` result:
server `keenable-mcp-server` `0.2.1`, protocolVersion `2025-06-18`, capabilities
`tools`/`resources`/`prompts`.

Free methods: `initialize`, `ping`, `notifications/*`, `tools/list`. Billed: `tools/call`, 1 credit
per call for either tool. A successful `tools/call` carries `_meta["keenable/usage"]`
(`sku`, `amount`, `credits`, `paid`, `credits_remaining`); harvest it.

Tool catalog (exact, verified 2026-11-16):

| Tool                 | Inputs                                                                                                                                                                                                                               | Constraints                                                                                                                                                                                                   |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `search_web_pages`   | `query` (required); `site`; `exclude_domains`; `acquired_after`; `acquired_before`; `published_after`; `published_before`; `query_time`; `snippet_max_length`; `max_results`; `session_id`; `output_format`; `mode`; `disable_cards` | `max_results` `1..50`; `snippet_max_length` `180..10000`; `session_id` <= 128 chars; `output_format` `text`/`json` (default text); `mode` `realtime`/`pro`; `exclude_domains` takes host entries, up to 1,000 |
| `fetch_page_content` | `url` (required); `max_chars`; `live`; `prompt`; `session_id`; `output_format`                                                                                                                                                       | `max_chars` >= 1; `prompt` <= 2000 chars; `session_id` <= 128 chars; `output_format` `text`/`json` (default text)                                                                                             |

## Zhihu REST

Base URL: `https://developer.zhihu.com` (default; overridable via the upstream config).

### Auth envelope

Both routes are GET only. Required headers: `Authorization: Bearer <access secret>`,
`X-Request-Timestamp` (Unix seconds, at most 10 minutes of skew), and
`Content-Type: application/json`. Hikari generates `X-Request-Timestamp` from the backend clock per
attempt and never forwards a client-supplied `Authorization` or timestamp. Live probe: a request
without a valid combination returns HTTP 200 with `Code=20001`.

### In-site search: `GET /api/v1/content/zhihu_search`

| Parameter | Type   | Required | Contract                                                                                                                                                                                                                            |
| --------- | ------ | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Query`   | string | yes      | Non-empty search keyword                                                                                                                                                                                                            |
| `Count`   | int32  | no       | Default 10, max 10; upstream truncates over-cap values, see the local guard below                                                                                                                                                   |
| `SortBy`  | string | no       | `field:direction:(min,max)`; fields `CommentCount`, `VoteUpCount`, `EditTime`, `CreateTime`; direction `asc`/`desc`/`none` (default desc); ranges optional, inclusive, bounds non-negative, min <= max; time bounds in Unix seconds |

Hikari local guard: `Query` missing or empty returns a local `400`; `Count` above 10 returns a
typed `400` with zero upstream calls (deliberate, instead of the upstream's silent truncation);
`Count <= 0` passes through so the upstream default of 10 applies; `SortBy` passes through
unvalidated.

Response: HTTP 200 with the native envelope. `Data.HasMore` is always `false` in the current
implementation, `SearchHashId` identifies the request, `Items[]` holds results, `EmptyReason` is
optional. Item fields (official doc): `Title`, optional `Headline`, `ContentType`, `ContentID`,
`ContentText`, `Url` (carries UTM parameters), `CommentCount`, `VoteUpCount`, `AuthorName`,
`AuthorAvatar`, `AuthorBadge`, `AuthorBadgeText`, `EditTime` (int32), `CreateTime` (int32),
optional `CommentInfoList[]` (`Content` each), `AuthorityLevel`, `RankingScore` (float32). Live
captures additionally show `AuthorSignature`. The facade must never strip `ContentType`,
`CreateTime`, or `RankingScore`, or any other upstream field.

### Global search: `GET /api/v1/content/global_search`

| Parameter  | Type   | Required | Contract                                      |
| ---------- | ------ | -------- | --------------------------------------------- |
| `Query`    | string | yes      | Non-empty search keyword                      |
| `Count`    | int32  | no       | Default 10, max 20; see the local guard below |
| `SearchDB` | string | no       | `all` (default), `realtime`, `static`         |
| `Filter`   | string | no       | Advanced filter grammar; URL-encode it        |

`Filter` grammar: `host` supports `==`/`!=` with double-quoted string values
(`host=="example.com"`; `zhihu.com` and its subdomains are unsupported, use `zhihu_search`);
`publish_time` supports `==`, `!=`, `>`, `>=`, `<`, `<=` with numeric Unix-second values; `AND` and
`OR` must be uppercase, `AND` binds tighter than `OR`, parentheses group.

Hikari local guard: `Count` above 20 returns a typed `400` with zero upstream calls; `Count <= 0`
passes through so the upstream default applies; `Filter` and `SearchDB` pass through unvalidated.

Response: HTTP 200 with the native envelope. `Data` carries `HasMore` and `Items[]`; live captures
also show `SearchHashId` although the doc example omits it. Item fields: `Title`, `ContentType`,
`ContentID`, `ContentText` (highlights use `<em>`), `Url` (carries UTM parameters), `CommentCount`,
`VoteUpCount`, `AuthorName` (anonymous shows as `知乎用户`), `AuthorAvatar`, `AuthorBadge`,
`AuthorBadgeText`, `EditTime` (int64), optional `CommentInfoList[]`, `AuthorityLevel` (`1` low to
`4` very high); live captures also carry `RankingScore`.

### Zhihu error table

Error and success share one HTTP-200 envelope: `{Code, Message, Data}`. A non-JSON body or a
missing `Code` falls back to the HTTP status default rule. The client always receives the HTTP 200
plus the native envelope; only `Code == 0` settles a credit.

| `Code`           | Meaning         | Hikari signal                            | Notes                                                                    |
| ---------------- | --------------- | ---------------------------------------- | ------------------------------------------------------------------------ |
| `0`              | success         | `Ok`                                     | Bills 1 credit via `business_success`                                    |
| `10001`          | parameter error | `ClientError`                            | Also returned for malformed quota `APIIDs`                               |
| `20001`          | auth failure    | `AuthFailed`                             | Missing or invalid `X-Request-Timestamp` also lands here (verified live) |
| `30001`          | rate limit      | `RateLimited { retry_after_secs: None }` | Never `Exhausted`                                                        |
| `90001`          | internal error  | `ServerTransient`                        |                                                                          |
| unknown non-zero | unknown         | `ClientError`                            | Never retry a new code blindly                                           |

### Quota: `GET /api/v1/quota`

Same auth headers as the search routes. Free: the docs state the call consumes no business quota.
Query: `APIIDs` optional, comma-separated, allowed at most once; omitted returns every configured
entry. Response: `Data[]` with `APIID`, `APIName`, `TotalQuota`, `TotalUsed`, `RemainingQuota` for
the current natural day; probe account values were `zhihu_search` 5000/day and `global_search`
5000/day.

Adapter rules: request `APIIDs=zhihu_search,global_search`; require both entries to be present
(missing entry is a typed `QuotaDataMissing`); store `quota_remaining` as the minimum
`RemainingQuota` across the two, `quota_limit` as the `TotalQuota` of the entry that produced the
minimum, and `quota_renews_at` as the next Asia/Shanghai midnight in epoch seconds at sync time.
A sample with `RemainingQuota == 0` marks the key exhausted until the next natural day.

### Local MCP facade

Ingress: `/zhihu/mcp`. Buffered; SSE-wrapped responses. `GET` with `Accept: text/event-stream` gets
`405`; an unknown tool gets `400`. Free: `initialize`, `ping`, `tools/list`, `notifications`.
Billed: `tools/call`, 1 credit only when the translated REST result is `Code == 0`; any non-zero
`Code` returns an MCP tool error and charges zero.

Tool catalog:

| Tool                  | Inputs                                                                                                         | Constraints                                                                                               |
| --------------------- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `zhihu_search`        | `query` (required string), `count` (optional integer), `sort_by` (optional string)                             | `count` `1..10`; translates to `GET /api/v1/content/zhihu_search`                                         |
| `zhihu_global_search` | `query` (required string), `count` (optional integer), `search_db` (optional enum), `filter` (optional string) | `count` `1..20`; `search_db` `all`/`realtime`/`static`; translates to `GET /api/v1/content/global_search` |

No other Zhihu tool is exposed, and the Access Secret never appears in a tool result or log.

## Credit table

One shared wallet. Every billed operation costs 1 local credit; failed calls are never billed.

| Provider | Operation                 | Credits op      | Local credits | Billed when                    |
| -------- | ------------------------- | --------------- | ------------- | ------------------------------ |
| keenable | REST search               | `search`        | 1             | Upstream `Ok` signal           |
| keenable | REST fetch                | `fetch`         | 1             | Upstream `Ok` signal           |
| keenable | MCP `search_web_pages`    | `search`        | 1             | `tools/call` succeeds upstream |
| keenable | MCP `fetch_page_content`  | `fetch`         | 1             | `tools/call` succeeds upstream |
| zhihu    | REST `zhihu_search`       | `zhihu_search`  | 1             | `Code == 0`                    |
| zhihu    | REST `global_search`      | `global_search` | 1             | `Code == 0`                    |
| zhihu    | MCP `zhihu_search`        | `zhihu_search`  | 1             | Translated result `Code == 0`  |
| zhihu    | MCP `zhihu_global_search` | `global_search` | 1             | Translated result `Code == 0`  |

Free (0 credits): `initialize`, `ping`, `notifications`, `tools/list`, resources reads; quota sync;
both validation probes; every client error. No credit settles on a Keenable `402`/`429`/`5xx` or
on a non-zero Zhihu `Code`.

## `NaturalDay` policy

- New `RecoveryPolicy::NaturalDay`, used by Zhihu only. Keenable keeps `UtcMonth`.
- Every active quota sync writes `quota_renews_at` = epoch seconds of the next Asia/Shanghai
  midnight.
- The recovery sweep flips an exhausted Zhihu key back to active when the backend clock reaches
  `quota_renews_at`, and writes the `auto_restore_active` audit row like the existing restore
  paths. The flip never happens on a UTC month boundary or a renewal date.
- The Keenable `402 insufficient_credits` exhaustion flips at the UTC month boundary or on a
  manual restore, never on `NaturalDay`.

## Operator runbook (never CI)

- Deployment egress: before enabling either provider's keys in a deployment, verify from the
  target network that the upstream host is reachable at the transport level (`api.keenable.ai`
  and `developer.zhihu.com`; a plain TLS handshake check qualifies and no API traffic is
  involved). This is an operator runbook step for the environment being deployed, never a CI job
  and never part of automated tests.
- Zhihu operator terms: an Access Secret is an account-level credential. The product offers
  Zhihu to end users through the shared wallet only, never automates multi-account rotation or
  free-tier farming, and operators must verify the resale terms of the Zhihu open platform
  before any public offering.
- Zero-cost validation: admin key validation uses only the free probes (Keenable missing-`query`
  400-vs-401; Zhihu `/api/v1/quota`). Billable validation probes are forbidden by the existing
  M13 rule.
