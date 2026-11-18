---
name: keenable-hikari-search
description: Search web pages through Tavily Hikari with the Keenable facade.
---

# Keenable Hikari Search

Use this skill when a task needs Keenable-native web search results through a Tavily Hikari
deployment.

## Workflow

1. Shape a focused query. Keenable understands `"exact phrase"`, `site:`, `OR`, `-`, and `( )`
   operators, plus `mode=pro` (default) or `mode=realtime` for fresher results.
2. Run a native POST with the Hikari token in the `X-API-Key` slot:

   ```bash
   ORIGIN="https://<your-hikari-host>"
   curl -s -X POST "$ORIGIN/api/keenable/v1/search" \
     -H "X-API-Key: th-<id>-<secret>" \
     -H "Content-Type: application/json" \
     -d '{"query": "coffee brewing ratio", "max_results": 5}'
   ```

3. Read the native JSON: `query`, `mode`, `results[]` with `title`/`url`/`snippet` and optional
   `published_at`, plus `usage` on keyed success. Cite result links when the answer depends on
   external facts.
4. Narrow results with `site`, `exclude_domains`, `published_after`/`published_before`, or
   `acquired_after`/`acquired_before` instead of filtering client-side.

## Boundaries

- `max_results` is 1..50 (default 10); `snippet_max_length` is 180..10000.
- The query body passes through unchanged, so unknown parameters surface as an upstream 400.
  Don't translate parameters for another provider's schema.
- Search only. Page fetching has its own route and skill (`keenable-hikari-fetch`).

## Hikari Notes

- Base URL: `https://<your-hikari-host>/api/keenable` mirrors `https://api.keenable.ai`.
- Auth slots: `X-API-Key: th-...` (native slot) or `Authorization: Bearer th-...`.
- Hikari strips client-supplied `X-API-Key` and injects the pooled upstream key; the token never
  reaches logs or upstream.
- Billing: 1 credit per successful search, per the shared wallet; 401/402/429/5xx and client
  errors settle 0. `usage.credits_remaining` is harvested passively from keyed successes.
- Limits: 10 requests per second per organization, shared by all the organization's keys.
- MCP clients can use `https://<your-hikari-host>/keenable/mcp` with the `search_web_pages`
  tool; see `keenable-hikari-mcp`.
