---
name: keenable-hikari-fetch
description: Fetch web pages as markdown through Tavily Hikari with the Keenable facade.
---

# Keenable Hikari Fetch

Use this skill when a task needs the content of a specific URL as markdown through a Tavily
Hikari deployment.

## Workflow

1. Take an absolute `http`/`https` URL and decide a `max_chars` bound (default 50000).
2. Run a native GET with the Hikari token in the `X-API-Key` slot:

   ```bash
   ORIGIN="https://<your-hikari-host>"
   curl -s "$ORIGIN/api/keenable/v1/fetch?url=https%3A%2F%2Fexample.com&max_chars=3000" \
     -H "X-API-Key: th-<id>-<secret>"
   ```

3. Read the native JSON: `content` (markdown, capped by `max_chars`), `truncated` and
   `total_chars` when the page was cut, optional `title`/`author`/`published_at`, and `usage`.
4. Use `live=true` only when you need a fresh fetch from the source, and `prompt` (max 2000
   chars) when the fetch should run a targeted extraction instead of returning the full page.

## Boundaries

- The `url` parameter must be an absolute `http`/`https` URL.
- The query string passes through unchanged; unknown parameters surface as an upstream 400.
- Fetch only. Web-wide searching has its own route and skill (`keenable-hikari-search`).

## Hikari Notes

- Base URL: `https://<your-hikari-host>/api/keenable` mirrors `https://api.keenable.ai`.
- Auth slots: `X-API-Key: th-...` (native slot) or `Authorization: Bearer th-...`.
- Hikari strips client-supplied `X-API-Key` and injects the pooled upstream key; the token never
  reaches logs or upstream.
- Billing: 1 credit per successful fetch, per the shared wallet; failures settle 0.
- MCP clients can use `https://<your-hikari-host>/keenable/mcp` with the `fetch_page_content`
  tool; see `keenable-hikari-mcp`.
