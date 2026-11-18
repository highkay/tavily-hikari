# Keenable and Zhihu provider implementation status

> The canonical contract stays in `./SPEC.md`; this file records implementation coverage,
> delivery status, and rollout facts so they do not scatter across PRs and git history.

## Current Status

- Implementation: todos 1-17 landed on `main` (base `e5e288cf`,
  `feat(web): add keenable and zhihu setup guides`); todo 19 is this docs/skills/env commit.
  Todos 18 (demo mode, i18n, stories), 20 (backend hardening), 21 (frontend hardening), and
  F1-F4 (final verification wave) are still open in `.omo/plans/keenable-integration.md`.
- Lifecycle: active
- Catalog note: provider registry entries, REST facades, both MCP surfaces, health matrix,
  quota sync, admin operations, read surfaces, admin/user console coverage, and setup guides
  are landed. Docs and skills packages close with this todo.

## Coverage / rollout summary

- Provider registry: `src/providers/{id,spec}.rs` (`Keenable`, `Zhihu`, `NaturalDay`
  recovery, `EgressPolicy::DirectAllowed` for both).
- Failure classification: `src/analysis/provider_failure_classification.rs` (Keenable
  `type`/`code` closed set; Zhihu HTTP-200 `Code` envelope).
- Privacy: per-provider header allowlists, `X-API-Key`/Zhihu Bearer strip and redaction,
  server-side `X-Request-Timestamp`, per-provider request timeout (Keenable 30s, Zhihu 20s).
- Stubs: `src/bin/mock_keenable.rs`, `src/bin/mock_zhihu.rs`; registered in
  `scripts/ci_backend_test_manifest.json`.
- Routing: `src/server/provider_facade.rs` plus kind catalogs in `src/analysis.rs`.
- REST facades: `src/tavily_proxy/proxy_provider_facade_keenable.rs`,
  `proxy_provider_facade_zhihu.rs` (passive credit harvest; `business_success` gating).
- MCP: `src/server/provider_mcp_keenable.rs` (hosted passthrough, `X-API-Key`),
  `src/server/provider_mcp_zhihu.rs` (local facade on the `LocalMcpFacade` framework).
- Health: `src/tavily_proxy/proxy_affinity.rs` wiring; `NaturalDay` flip in
  `src/store/key_store_token_logs.rs`.
- Quota sync: `proxy_quota_sync_provider_adapters.rs` (Zhihu daily adapter) and the
  `quota_sync/zhihu` scheduler lane in `src/server/schedulers_provider_quota_sync.rs`.
- Admin: key create/list/filter/validate/sync in
  `src/server/handlers/admin_resources/api_keys_and_logs.rs` and `admin_auth.rs`.
- Read surfaces: `src/store/key_store_request_logs_provider_read.rs` and the 8 new
  request kinds.
- Web: `web/src/api/providers.ts`, demo fixtures (`demoKeenable.ts`, `demoZhihu.ts`),
  admin key/quota panels, user-console provider guides, en+zh copy.
- Docs/skills (this todo): README en/zh, docs-site en/zh provider pages plus the three
  shared pages, `docs/multi-provider-search-proxy.md`, this SPEC trio, `.env.example`
  (already complete), compose env samples, AGENTS.md stub rule, and the six skill packages.

## Remaining Gaps

- Todo 18: demo route handlers for `/api/keenable/*`, `/api/zhihu/*`, `/keenable/mcp`,
  `/zhihu/mcp`, plus i18n registrations and stories (plan Wave 4).
- Todo 20: shared-wallet matrix, health matrix, facade contract matrix, MCP E2E privacy
  test, and manifest/shard gates.
- Todo 21: full frontend battery and release-surface assertion.
- F1-F4: compliance audit, quality review, manual QA, ideal-state fidelity.

## Related Changes

- Rust (new): `src/tavily_proxy/proxy_provider_facade_{keenable,zhihu}.rs`,
  `src/server/provider_mcp_{keenable,zhihu}.rs`, `src/bin/mock_{keenable,zhihu}.rs`.
- Rust (modified): `src/providers/*`, `src/analysis/*`,
  `src/tavily_proxy/{mod.rs,proxy_provider_facade.rs,proxy_quota_sync_provider_adapters.rs, proxy_affinity.rs}`, `src/server/*`, `src/store/*`, `src/main.rs`.
- Tests: `tests/provider_facades_contract.rs`, `tests/provider_mcp_e2e.rs`,
  `src/server/tests/provider_*`, `src/tests/provider_*`, route/classifier/health matrices.
- Web: `web/src/api/{providers.ts,demoKeenable.ts,demoZhihu.ts}`,
  `web/src/user-console/providerGuides.tsx`, admin panels, i18n.
- Docs/skills: `docs/adr/0005-multi-provider-upstream-abstraction.md` (todo 1),
  `docs/multi-provider-search-proxy.md`, this SPEC trio, README en/zh, docs-site en/zh,
  `skills/{keenable,zhihu}-hikari-*`, compose env samples, AGENTS.md.

## Validation

- `bunx --bun dprint check` on every changed Markdown (this todo's gate)
- `cd docs-site && bun run build` (this todo's gate)
- Sidebar assertion: both locales list Keenable and Zhihu (this todo's gate)
- no production-test instruction grep (this todo's gate)
- Evidence: `.omo/evidence/keenable-integration/task-19-keenable-integration.txt`

## References

- `./SPEC.md`
- `./HISTORY.md`
- `../../multi-provider-search-proxy.md`
- `../../adr/0005-multi-provider-upstream-abstraction.md`
- `.omo/plans/keenable-integration.md`
