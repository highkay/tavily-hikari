# Keenable and Zhihu provider evolution history

> This file records the turn-by-turn decisions that explain why the integration looks the way it
> does. Single-task logs stay in `.omo/evidence/`; the canonical contract stays in `./SPEC.md`.

## Decision Trace

- 2026-11-16: live probes against both providers captured (Keenable search/fetch/MCP envelope,
  the closed seven-value `type` set, `usage.credits_remaining`; Zhihu search/global search
  HTTP-200 `Code` envelope, timestamp auth, `/api/v1/quota`, SSE-only official MCP). Captures
  live in `.omo/evidence/keenable-integration/probes/`.
- 2026-11-16 (todo 1, `docs(adr): record keenable and zhihu provider contracts`): ADR 0005
  amended with the `## Keenable and Zhihu` section and `docs/specs/keenable-zhihu-providers/SPEC.md`
  created as the contract of record. Decisions recorded: Keenable hosted-MCP passthrough with
  `X-API-Key`; Zhihu local MCP facade (official MCP is SSE-only); `NaturalDay` recovery;
  zero-cost validation probes; 1 local credit per successful upstream call.
- 2026-11-16 to 2026-11-19 (todos 2-17): registry and seam arms, envelope classifiers, header
  policy and timeouts, stub upstreams, route/kind/config tables, both REST facades, both MCP
  surfaces, health wiring, passive + active quota sync, admin key operations, read surfaces,
  and the web API/admin/guide layers landed one commit per todo on `main`.
- 2026-11-19 (this todo): docs, skills packages, env samples, and the operator terms note;
  `docs: document keenable and zhihu providers`.
- 2026-11-19 (release): hardening matrix + manifest (todo 20), frontend battery (todo 21), and
  the F4-spawned `X-RateLimit-*` passive harvest (todo 22, meta KV `keenable_rate_limit_v1`)
  landed; F1-F4 final gates all APPROVE; `main` pushed at `1d769ea1`, image
  `0.0.0-fork.20261119.1d769ea` deployed to the fnOS host with one Keenable key and one Zhihu
  secret loaded from `KEENABLE_API_KEYS`/`ZHIHU_API_KEYS`; both providers verified with a live
  smoke search each.

## Key Reasons / Replacements

- Keenable errors form a closed seven-value `type` set. Branching on `type`/`code`, never on
  `error`/`message` text, keeps health mapping stable; `target_error` is always a client error
  and never marks a key.
- Zhihu's success and failure share one HTTP-200 envelope, so billing and health are gated on
  `business_success` (`Code == 0`) instead of the HTTP status; a non-zero `Code` still reaches
  the client unchanged but settles zero credits.
- Zhihu's official MCP is SSE-only and held connections open in probing, so a local facade on
  the existing `LocalMcpFacade` framework is the supported surface; the official MCP is not
  proxied.
- Zhihu quota resets on a natural day, not a UTC month or renewal date, which required the
  `NaturalDay` recovery policy (next Asia/Shanghai midnight, stored as epoch seconds).
- Keenable has no active balance endpoint in scope, so `usage.credits_remaining` is harvested
  passively from keyed REST successes and MCP `_meta`; admin sync returns the last snapshot.
- A Zhihu Access Secret is an account-level credential. The product exposes Zhihu through the
  shared wallet only, never automates multi-account rotation or free-tier farming, and the
  operator terms note requires verifying resale terms before any public offering.
- Both providers join the existing cross-key retry kernel as provider REST read surfaces;
  client errors (Keenable `target_error`/`invalid_request_error`, Zhihu `10001` and unknown
  codes) never rotate keys or mark them.

## Follow-ups (explicitly recorded, outside this todo)

1. Todo 18: demo-mode route handlers for the four provider paths, remaining i18n
   registrations, and Storybook stories (`web/src/api/demo.ts`).
2. Todo 20: shared-wallet and health matrices, full facade contract matrix, MCP E2E privacy
   assertions, and manifest registration.
3. Todo 21: complete frontend battery and release-surface verification.
4. F1-F4: compliance audit, quality review, manual QA against the two mock upstreams, and
   ideal-state fidelity walk.

## References

- `./SPEC.md`
- `./IMPLEMENTATION.md`
- `../../multi-provider-search-proxy.md`
- `.omo/drafts/keenable-integration.md`
- `.omo/plans/keenable-integration.md`
- `.omo/evidence/keenable-integration/probes/`

## Legacy Identity

- None (new topic, no historical Legacy ID).
