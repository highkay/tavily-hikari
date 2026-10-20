# ADR 0009: Cross-Key Retry on Eligible Upstream Failures

## Status

Accepted

## Context

A Hikari deployment pools many upstream keys per provider (Tavily, SerpApi, Exa, Brave) behind one
native HTTP facade and one MCP ingress. One logical request leases one key; when that key came back
rejected - 429, 432, quota, auth block - the rejection travelled to the client byte-for-byte even
while the pool held healthy capacity. Operators saw single-key rejections with a full pool and could
not tell from the logs whether the pool or the request was at fault.

The upstream-agnostic rebalance SPEC first froze the answer as "no cross-key retry at all": an
ordinary POST/JSON request is not assumed idempotent, so replaying one that upstream already
executed would double-bill and duplicate side effects. The wedge that reopens the question is
provability: a rate limit, an exhaustion flip, or an auth block proves the upstream refused
_before_ doing work, so replaying exactly that attempt on another key cannot double-execute and
cannot double-bill. Failures whose execution is unprovable - timeouts, 5xx, unknown - stay
single-attempt under that original contract.

The plan `provider-key-429-retry` froze a first form of that wedge, an "explicit rejections only"
whitelist. The 2026-10-16 user-approved amendment supersedes that frozen form: eligibility widens
to key-scoped rejections plus key-scoped transient failures, request-shaped 4xx and the
`/research` and `/crawl` surfaces are excluded explicitly, and the Exa vetoes stay. This ADR records
the amended boundary, which is why it is titled for eligibility rather than for "explicit
rejection only".

## Decision

- Eligibility (cross-key retry contract in
  `docs/specs/upstream-agnostic-api-rebalance/SPEC.md`; kernel
  `src/tavily_proxy/proxy_key_retry.rs`):
  - Retryable, arm 1 - a provably-unexecuted key-scoped rejection: 429 rate limit, 432 exhaustion,
    the 401/403 auth-block family (including its bare forms), and 402 exhaustion. The decision
    reads the _effective_ status, so an HTTP 200 whose envelope body says `status: 432` counts, and
    a normalized signal outranks the numeric code (Brave reports an invalid subscription token as
    422 and is classified as auth-block).
  - Retryable, arm 2 (2026-10-16 amendment) - a key-scoped transient failure: an upstream 5xx
    (`ServerTransient`), a transport send error, or a timeout. None of them is caused by the
    request, so one bounded try on another key beats surfacing the failure.
  - Never retryable - a request-shaped 4xx (400/404/406/422/...): the request itself is wrong, so
    rotating a key cannot fix it and marking the key would only burn the pool. The executor must
    neither mark nor rotate on that reason.
  - Never replayed - `/research` (POST submission and GET result poll) and `/crawl`, under either
    arm: replay safety comes from the request surface, not from the trigger class. MCP surfaces
    never enter this contract (pinned sessions migrate per the SPEC migration section; Exa/Brave
    local MCP stay single-attempt in v1).
  - Vetoed - Exa 429 and Exa team-scoped 402 (`TEAM_BUDGET_EXCEEDED`): a team-level limit another
    key cannot lift. Only an explicitly key-scoped tag (`NO_MORE_CREDITS`,
    `API_KEY_BUDGET_EXCEEDED`) makes an Exa 402 replayable; missing tag evidence keeps the veto.
  - A Retry-After beyond the fast-path window is never absorbed: the kernel never sleeps, so the
    first response is served unchanged.
- Budget: at most 2 upstream attempts per logical request (one retry) inside a 5s deadline; zero
  sleep; immediate replay only when the armed Retry-After is at most 2s, with the armed and
  signal-parsed values unioned by the longer one. Tuning is env-only from one override point -
  `KEY_RETRY_MAX_ATTEMPTS` (clamped to 2; 1 disables retry), `KEY_RETRY_DEADLINE_SECS`,
  `KEY_RETRY_RATE_LIMIT_FAST_PATH_SECS`. v1 deliberately does not go through SystemSettings or the
  admin UI, so the budget stays reversible without an admin-surface change.
- Pool outcome: when no eligible key exists (empty pool or a strict-acquire miss), the buffered
  first-hit response returns byte-for-byte - status, Retry-After header, body. The 502 path is
  never reused and the response is never rewritten.
- Acquisition: the replacement key comes from a strict predicate - the rejected key and every key
  already judged dead in this request are excluded, cooling/quarantined/exhausted keys are skipped,
  there is no fallback to a cooling key - over the existing candidate hierarchy and ordering,
  filtered by the same backoff scope the request armed.
- Marking and billing: each upstream attempt produces one visible `request_logs` row, and the chain
  is marked only through `fallback_reason`: `upstream_key_retry:attempted` (the losing first hit),
  `upstream_key_retry:succeeded` (final success), `upstream_key_retry:rejected` (final rejection),
  `upstream_key_retry:skipped_no_eligible_key` (first hit served because no eligible key existed).
  A single-attempt result is not a chain and carries no marker. Billing settles once on the final
  response through the unchanged settlement path.
- Concurrency acceptance: two racing logical requests may be handed the same replacement key. The
  strict acquire is a read plus an LRU touch, not a claim, so it offers no mutual exclusion, and
  the plan accepts that - the cost of the race is one extra upstream call, never a correctness
  break or a second settlement. An atomic claim (or a lease table) is deferred to a future ADR.

## Consequences

Rejected alternatives, recorded so a later decision reopens them explicitly:

- `suppressed_retry_shadow` suppression rows: the visibility flag already exists, and hiding the
  losing attempt would keep recent-request views at one row per logical request. Rejected - it
  would break "one visible row per upstream attempt", which is what lets an operator see the retry
  chain and the pool's behavior, and it would drag the bucket rollup into the retry path. Both rows
  stay visible and carry the chain markers.
- Atomic claim for the replacement pick: would need a claim-fenced lifecycle and either a new table
  or a schema migration. Rejected for v1 - the accepted race window above bounds the damage; it is
  deferred to its own ADR.
- Auto-removal of a key on any non-429 4xx: rejected - a request-shaped 4xx blames the request, so
  deleting or disabling a healthy key would burn pool capacity on client bugs. Instead the layered
  marking stays the only marking surface: quarantine keywords on 401/403 -> quarantine; bare
  401/403 -> the long transient cooldown (120s base, 900s ceiling, Retry-After honored); 432 ->
  monthly exhaustion; 429 -> the short re-selectable cooldown (30-300s clamp).
- A UI / SystemSettings switch for the retry budget in v1: rejected - the budget is an operator
  tuning point, not a rollout gate, and the env-only surface keeps the change reversible; a UI
  switch is a separate spec, and adding it now would collide with the in-flight admin/log effect
  work.

Explicit assumptions, each handled safely under either truth:

- Exa 429 and team-scoped 402 scope is unconfirmed upstream: the vendor documentation does not state
  whether Exa's 429 is key-scoped or team-scoped, and `TEAM_BUDGET_EXCEEDED`'s blast radius is only
  implied by the tag name. The kernel vetoes both conservatively. If the 429 later proves
  key-scoped, rotation would have helped and today we merely serve the first hit; key-scoped 402
  tags are still replayed today, so no truth of the assumption can turn a team limit into a replay.
- Tavily bare 401/403 existence is unprobeable in production: repository policy forbids pointing
  tests at production upstreams, so the classifier cannot confirm whether Tavily ever emits a bare
  401/403 without a quarantine keyword. Bare 401/403 is mapped into the auth-block family with the
  longer cooldown. If Tavily never emits them, the branch is dead but harmless - the 401 arm sits
  last and can never shadow a more precise diagnosis, and the 403 arm keeps exactly the position
  the old `upstream_unknown_403` held; if Tavily does emit them, the long cooldown is the correct
  lane.

Risk register:

- (a) HA dual-active peer-row durability: the session rebind's UPDATE is a silent no-op when the
  session row is owned by the peer node - the write returns `Result<(), ProxyError>` without
  exposing rows-affected, so the caller cannot tell. The peer's row keeps pointing at the dead key,
  and every follow-up re-runs the whole migration chain (about 3 upstream calls instead of 1,
  indefinitely) until the peer row is rewritten. The durable fix - returning rows-affected so the
  caller can fall back - is deferred and recorded as a known limitation in the SPEC migration
  section.
- (b) Exa padded-tag whitespace tolerance: `ExaBudgetTag::from_tag` trims and case-folds, so a
  padded `NO_MORE_CREDITS` now classifies `Exhausted` where the pre-trim matcher demoted it to
  `ClientError`; a padded 402 therefore flips its key to exhausted instead of surfacing as a
  generic client error. Pinned by test.
- (c) Pool first-pick nondeterminism under id ties: the pool pass orders candidates by
  `last_used_at ASC, id ASC` and key ids are random (`nanoid!(4)`), so among otherwise-equal
  candidates the first pick is arbitrary. Tests must not assert which tied key wins - they pin
  attempt counts and identify keys by exclusion.
- (d) The zero-sleep pin couples to the cooldown floor: the HTTP zero-sleep case asserts the
  mock-stamped gap between attempts stays under 20s, and it discriminates only because the shortest
  cooldown those cases arm is the 30s ladder floor (`MCP_SESSION_INIT_BACKOFF_MIN_SECS` in
  `src/lib.rs`). If that floor ever drops below 20s, a wait-out implementation could pass the pin;
  the bound must be re-derived with the constant.
- (e) First-failure exile and penalized-only recovery probes (2026-10-19 incident class): a
  forward-proxy node was marked unavailable on its first failed attempt, and the maintenance loop
  only probed penalized nodes, so an unavailable node whose weight stayed intact never received a
  recovery probe. Only a process restart or a settings rewrite reset that state, which let one
  transient failure exile a healthy node from rotation. On 2026-10-19 this dead-zoned two healthy
  proxies for about 18 hours while Brave, GFW-blocked on the direct path, had no egress at all.
  Probe-target health also masqueraded as proxy health: the mihomo gateway's example.com route
  answered the probe URL with a ~5s 502, which the old predicate read as the proxy being dead.
  The wave-1 fixes replace this with a consecutive-failure threshold
  (`FORWARD_PROXY_UNAVAILABLE_AFTER_FAILURES`, default 3, clamp 1-10) before a node goes
  unavailable, recovery probes covering every unavailable node (not only penalized ones),
  any-HTTP-response probe success, and env-tunable `FORWARD_PROXY_PROBE_URL` (default
  `http://example.com/`) and `FORWARD_PROXY_VALIDATION_TIMEOUT_SECS` (default 5, clamp 2-60).
  Evidence: `.omo/evidence/prod-request-errors/`.

Conflict disclosure:

- No new effect codes. The retry chain rides `fallback_reason` only, so the `key_effect_*` /
  `binding_effect_*` / `selection_effect_*` allow-lists in the `log-effect-bucket-split` SPEC stay
  unchanged and the in-flight bucket split does not collide with this decision. Retry rows carry
  exactly the marking of an ordinary attempt (the rejection's own family effect), never a
  retry-specific code.
