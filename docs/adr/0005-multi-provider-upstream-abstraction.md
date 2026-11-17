# ADR 0005: Multi-Provider Upstream Abstraction

## Status

Accepted

## Context

Tavily Hikari multiplexes Tavily keys behind one MCP ingress and one native HTTP facade. The
product will now stand three more search providers beside Tavily: SerpApi, Exa, and Brave Search.
Their contracts differ in the exact places the current code assumes Tavily, so a single hard-coded
provider can no longer describe ingress, key injection, health, quota, and billing.

SerpApi authenticates REST calls with the `api_key` query parameter, not a header. `GET /account`
is documented free and uncounted and returns `plan_searches_left`, `total_searches_left`,
`searches_per_month`, and `plan_renewal_date`, which mirrors the existing Tavily usage sync. Its
`429` is dual-semantic: monthly exhaustion arrives as `"Your account has run out of searches."`,
while a throughput `429` uses a different message and is transient. SerpApi hosts an MCP server at
`mcp.serpapi.com` that accepts `Authorization: Bearer <key>`, and its `search`, `search_table`, and
`search_dashboard` tool names collide with Tavily's canonical classifier names. Its terms
restrict resale and multi-account patterns (sections 2 and 6).

Exa authenticates with `x-api-key` (or Bearer) against `api.exa.ai`. Exhaustion is a `402` whose
tag is `NO_MORE_CREDITS`, `API_KEY_BUDGET_EXCEEDED`, or `TEAM_BUDGET_EXCEEDED`. Exa exposes no live
remaining-balance API: only a separate team-management usage history host, gated per team, with no
"credits left" read. Its hosted MCP at `mcp.exa.ai` is Cloudflare-fronted, and a direct POST was
observed as `"Sorry, you have been blocked"`; its server also reports analytics to a third party
(`api.agnost.ai`). That analytics flow conflicts with this product's high-anonymity posture. Exa's
license is non-transferable and non-sublicensable per authorized user.

Brave authenticates with the `X-Subscription-Token` header against `api.search.brave.com/res/v1`.
An invalid key returns HTTP `422` with `SUBSCRIPTION_TOKEN_INVALID`, not `401` (live-probed);
quota exhaustion returns `402` with `QUOTA_LIMITED`. Brave publishes `X-RateLimit-*` headers that
cover both a second window and the month window (`w=2592000`), so the monthly remaining amount can
be read from any response without a usage endpoint. Brave hosts no MCP server; its official MCP is
self-hosted only. Its terms prohibit retaining search-result data and limit key sharing to
affiliates and contractors who need to know (section 3(b)(v)/(xii), section 4(a)).

All three providers' terms restrict reselling, key pooling, and multi-account rotation while this
product resells metered access through tokens and Linux.do credit. The differences above are real
seams, not cosmetic naming: URL building, key injection, the exhaustion gate, the tool namespace,
and the usage-sync source. The decision below records how those seams are absorbed without moving
Tavily behavior. Substantive terms are recorded in `CONTEXT.md` under Multi-Provider Upstream Terms.

## Decision

- Add one additive registry of `ProviderSpec` entries keyed by `ProviderId` (`Tavily`, `Serpapi`,
  `Exa`, `Brave`). Each spec carries the REST base default, the hosted MCP upstream and MCP mode,
  the upstream key-injection style, the client auth slots, the per-operation credit table, the
  quota-sync capability, and the recovery policy. The Tavily spec records today's constants as
  metadata only; it never rewires the existing Tavily request path, allowlists, or defaults.
- Route by path prefix. `/serpapi/mcp` is a passthrough to the hosted SerpApi MCP with Bearer
  injection and session-id translation. `/exa/mcp` and `/brave/mcp` are local MCP facades. Native
  HTTP facades are `/api/serpapi/search`, `/api/exa/{search,contents,answer}`, and
  `/api/brave/res/v1/{web/search,news/search,llm/context}`. Every facade accepts the Hikari `th-`
  token in the provider-native auth slot and as `Authorization: Bearer`.
- Keep one shared credit wallet per token and add a per-provider credit table for billed
  operations: serpapi search `1`; exa search `1`, contents `1`, answer `2`; brave web `1`, news
  `1`, llm_context `2`. Initialize, ping, tools/list, and resources remain non-billable.
- Scope the key pool by provider. Acquire, affinity, transient backoff, quarantine, and health
  transitions never lease a key across providers; a request only ever uses a key for the provider
  it addresses.
- Classify exhaustion per provider instead of assuming the Tavily `432`. SerpApi `401`/`403` are
  auth failures, a `429` carrying `"run out of searches"` is exhausted with renewal-date recovery,
  and any other `429` is an hour-scoped transient backoff. Exa `401` is an auth failure, a `402`
  tagged `NO_MORE_CREDITS`, `API_KEY_BUDGET_EXCEEDED`, or `TEAM_BUDGET_EXCEEDED` is exhausted with
  success-restore or manual recovery, and `429` honors `Retry-After`. Brave `422`
  `SUBSCRIPTION_TOKEN_INVALID` is an auth failure, `402 QUOTA_LIMITED` is exhausted on the month
  window, and `429` is a short backoff. Low-quota depletion stays Tavily-only.
- Keep segmented reconciliation, the `accessToken` HMAC pseudonym, and `X-Project-ID` injection
  Tavily-only. No third-party upstream ever receives the pseudonym, and reconciliation and
  research-drain jobs stay guarded to Tavily keys.
- Split the migration. One main-ledger migration `v45` owns the `provider` columns for `api_keys`,
  `auth_token_logs`, and `mcp_sessions`. A separate observability-sidecar ALTER path adds
  `request_logs.provider` idempotently. The two mechanisms are never conflated.
- Sync quota from what each provider offers. SerpApi uses an active `/account` adapter inside the
  existing sync lanes. Brave uses a passive header harvest. Exa uses none, because no live balance
  read exists.
- Declare each provider's direct-egress stance in the registry (`EgressPolicy`). Brave is
  `ProxyRequired`: `api.search.brave.com` is DNS-poisoned and TCP-blocked on the GFW-side networks
  this fork serves, so a direct attempt can only fail as a transport error while hiding the real
  cause (every proxy node exiled). Tavily, SerpApi, and Exa stay `DirectAllowed`. The stance is
  consumed by the forward-proxy plan builder and its post-plan direct fallback; it degrades to
  `DirectAllowed` when the deployment has no forward proxy configured at all, because direct is
  then the only possible egress. An exhausted pool for a `ProxyRequired` provider fails before any
  upstream socket exists and records `forward_proxy_egress_unavailable`, never
  `transport_send_error`.
- The registry stance is the ONLY source of egress policy; there is no per-deployment override.
  An override env var (`PROVIDER_EGRESS_OVERRIDES`) was built, tested, and then removed: nothing
  set it, and it let one deployment switch off the exact guard the 2026-10-22 audit added. The
  accepted cost is that a deployment which has forward proxies configured AND can also reach
  `api.search.brave.com` directly still cannot use direct for Brave - it must drop the proxy
  config entirely, which would then cost the providers that genuinely need the proxy. Change
  `BRAVE.egress` in the registry if that deployment ever becomes real.
- The gate takes the provider as a PARAMETER rather than resolving it from the routing subject.
  Every caller already holds one (`request.provider` in the facades, a literal in the Tavily-only
  seams), and the previous `SELECT provider FROM api_keys WHERE id = ?` ran up to four times per
  direct-candidate request while silently answering `Tavily` for a subject that was not an
  `api_keys.id` at all - a fail-open default on the one field that decides whether a request may
  leave through the blocked egress.
- `failure_kind` is stamped from the typed `ProxyError` at the audit-row call site
  (`failure_kind_from_proxy_error`) instead of being re-derived from the error's `Display` text.
  The text classifier stays as the fallback for every other row, but a wording change can no
  longer silently downgrade these diagnoses to `transport_send_error`.

## Keenable and Zhihu

Recorded 2026-11-16 from live probes against both providers and their official docs. The probe
captures live under `.omo/evidence/keenable-integration/probes/`, and the operative contract for the
todos that follow is `docs/specs/keenable-zhihu-providers/SPEC.md`.

Keenable serves web search and page fetch from `api.keenable.ai` with the `X-API-Key` header
(`Authorization: Bearer <key>` is also accepted, and `X-API-Key` wins when both are sent). The REST
surface is `POST /v1/search` and `GET /v1/fetch`, and the hosted MCP server is `POST https://api.keenable.ai/mcp`, which answers Streamable HTTP JSON with exactly `search_web_pages` and
`fetch_page_content`. Every keyed success returns a `usage` object (`sku`, `amount`, `credits`,
`paid`, `credits_remaining`), so `usage.credits_remaining` is the passive org balance; MCP tool
results carry the same object under `_meta["keenable/usage"]`. Errors are one flat JSON body whose
`type` is a closed set of seven values. The mapping is: `authentication_error` to auth-fail;
`billing_error` on `402` to exhausted and on `451` to client error; `permission_error` to auth-fail
only for `credentials_forbidden`/`forbidden` and otherwise to client error; `target_error` to client
error regardless of status, never a key penalty; `rate_limit_error` to rate-limited from
`retry_after_seconds` or `Retry-After`; `api_error` to transient; `invalid_request_error` to client
error; an unknown or missing `type` falls back to the HTTP status default. Authenticated traffic is
capped at 10 requests per second per organization, the free allowance is 100,000 requests per month
and resets monthly, then $4 per 1,000 requests.

Zhihu serves in-site and web-wide search from `developer.zhihu.com` as
`GET /api/v1/content/zhihu_search` and `GET /api/v1/content/global_search`, authenticated with
`Authorization: Bearer <access secret>` plus a required `X-Request-Timestamp` in Unix seconds with
at most 10 minutes of skew; the proxy generates the timestamp server-side. Success and failure both
arrive as HTTP 200 with a `{Code,Message,Data}` envelope: `0` ok, `10001` parameter error, `20001`
auth failure, `30001` rate limit, `90001` internal error; an unknown non-zero `Code` is a client
error. `Count` caps at 10 for `zhihu_search` and 20 for `global_search`; a client `Count` above the
cap is rejected locally with a typed 400 instead of the upstream's silent truncation, `Count <= 0`
passes through to the upstream default of 10, and `SortBy`/`Filter`/`SearchDB` pass through
unvalidated. `GET /api/v1/quota` is free and returns `TotalQuota`/`TotalUsed`/`RemainingQuota` per
API for the current natural day, which makes it both the active quota source and the zero-cost
validation probe. Zhihu's official MCP server is SSE-only; a plain request held the connection open
in probing, so the supported MCP surface is a local facade, not a passthrough.

The decisions above extend as follows:

- Register both providers. Keenable carries `X-API-Key` as both its client-native auth slot and its
  upstream injection slot (stripped from client input before logging, then re-injected from the
  pool), plus `UtcMonth` recovery and a hosted MCP passthrough at `/keenable/mcp`. Zhihu carries
  `Bearer` injection with no client-native header slot (the universal Hikari Bearer holds the `th-`
  token), a new `RecoveryPolicy::NaturalDay` whose `quota_renews_at` is the next Asia/Shanghai
  midnight, and a local MCP facade at `/zhihu/mcp` with `zhihu_search` and `zhihu_global_search`.
  An exhausted Zhihu key flips active at that boundary and never on a UTC month or renewal-date
  boundary.
- Bill 1 local credit per successful upstream call on the one shared wallet: Keenable search and
  fetch, Zhihu `zhihu_search` and `global_search`, and each MCP `tools/call`. Zhihu settles only
  when `Code == 0` despite the HTTP 200; `initialize`, `ping`, `tools/list`, and notifications stay
  free.
- Harvest Keenable passively from `usage.credits_remaining` and `_meta["keenable/usage"]`, and sync
  Zhihu actively from the free `/api/v1/quota`, storing the minimum remaining across the two search
  APIs and the next natural-day reset. Both egress stances are `DirectAllowed`, and the registry
  remains the only source of egress policy.
- Validate keys at zero cost: Keenable probes `POST /v1/search` with a missing `query` (a `400 missing_parameter` proves the key works, a `401` proves it does not), Zhihu probes the free
  `/api/v1/quota` (`Code=0` valid, `20001` invalid).
- Offer Zhihu to end users through the shared credit wallet with an operator terms note, and never
  through multi-account rotation or farming. The account-scoped Access Secret stays in the operator
  key pool behind the `th-` token.

Nothing on the existing Tavily, SerpApi, Exa, or Brave paths changes: their allowlists, credit
tables, golden behavior, and the Tavily-432 depletion gate stay byte-stable, and the stub-only
upstream rule extends to the two new providers.

## Consequences

- Exa hosted-MCP passthrough is deferred. The local facade is the only Exa MCP surface for now:
  the hosted server's Cloudflare obstruction was observed directly, and its third-party analytics
  conflict with the high-anonymity posture. The deferral is recorded here so a later decision
  reopens it explicitly.
- Brave's data-retention prohibition is an operator-policy note only. The existing body-retention
  profiles are the lever; no per-provider retention code is added.
- All three providers need an operator legal note on resale and pooling: SerpApi sections 2 and 6,
  Exa's non-transferable license, and Brave section 3(b)(v)/(xii) plus section 4(a). Pooling one
  organization's own keys within purchased quota is the documented boundary.
- Tavily stays byte-stable. The golden Tavily suites, the three existing header allowlists, and the
  existing env defaults must pass unchanged except for the authorized schema-inventory vector.
- New providers are proven against stub upstreams only. The existing "never hit production
  endpoints" rule extends to SerpApi, Exa, and Brave.
- Provider logic lives in ProviderSpec and classifier tables, not scattered conditionals, and new
  files stay inside the repository source-line budgets.
