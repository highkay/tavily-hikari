---
name: keenable-hikari-mcp
description: Use Keenable's hosted MCP tools through Tavily Hikari's passthrough.
---

# Keenable Hikari MCP

Use this skill when an MCP client should call Keenable's official tools (`search_web_pages`,
`fetch_page_content`) through a Tavily Hikari deployment.

## Workflow

1. Point the MCP client at the Hikari ingress with bearer auth:

   ```json
   {
     "mcpServers": {
       "keenable_hikari": {
         "url": "https://<your-hikari-host>/keenable/mcp",
         "headers": { "Authorization": "Bearer th-<id>-<secret>" }
       }
     }
   }
   ```

   The `X-API-Key: th-...` native slot works too.

2. Call the tools with Keenable's native input schemas. `search_web_pages` takes `query`
   (required), `site`, `exclude_domains`, date filters, `max_results` 1..50,
   `snippet_max_length`, `mode`, `output_format`, `session_id`, `disable_cards`.
   `fetch_page_content` takes `url` (required), `max_chars`, `live`, `prompt`, `session_id`,
   `output_format`.

3. Read tool results directly; successful calls carry `_meta["keenable/usage"]` with
   `credits_remaining`.

## Boundaries

- The transport is buffered HTTP JSON-RPC (Streamable HTTP). There is no streaming or SSE.
- `/keenable/mcp/*` subpaths are rejected locally and a GET that opens an event stream gets
  405: use a standard MCP-over-HTTP client.
- Sessions are provider-scoped. A session id minted on `/serpapi/mcp` or `/mcp` is not valid
  here.
- The catalog is exactly the two tools above; don't invent additional tools.

## Hikari Notes

- `initialize`, `ping`, `notifications/*`, and `tools/list` are free.
- Billing: 1 credit per successful `tools/call` for either tool, per the shared wallet.
- Hikari injects the pooled upstream key in `X-API-Key` and never leaks it into logs or
  responses.
