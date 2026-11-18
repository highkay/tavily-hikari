---
name: zhihu-hikari-mcp
description: Use Zhihu search tools through Tavily Hikari's local MCP facade.
---

# Zhihu Hikari MCP

Use this skill when an MCP client should call Zhihu search (`zhihu_search`,
`zhihu_global_search`) through a Tavily Hikari deployment.

## Workflow

1. Point the MCP client at the Hikari ingress with bearer auth:

   ```json
   {
     "mcpServers": {
       "zhihu_hikari": {
         "url": "https://<your-hikari-host>/zhihu/mcp",
         "headers": { "Authorization": "Bearer th-<id>-<secret>" }
       }
     }
   }
   ```

2. Call the two tools with the facade's input schemas. `zhihu_search` takes `query` (required),
   `count` 1..10, and `sort_by`. `zhihu_global_search` takes `query` (required), `count` 1..20,
   `search_db` (`all`, `realtime`, `static`), and `filter`.

3. Tool results carry the native `{Code, Message, Data}` envelope as an SSE `data:` frame. A
   non-zero `Code` arrives as an MCP tool error with zero credits billed.

## Boundaries

- The facade is local: Hikari translates each `tools/call` to the matching Zhihu REST endpoint.
  Zhihu's official SSE-only MCP is not used.
- A GET that tries to open an event stream on the endpoint gets 405; use a standard
  MCP-over-HTTP client. Unknown tools get 400.
- Sessions are provider-scoped. A session id minted on `/mcp` or another provider is not valid
  here.
- The catalog is exactly the two tools above; don't invent additional Zhihu tools.

## Hikari Notes

- `initialize`, `ping`, `tools/list`, and `notifications` are free.
- Billing: 1 credit per `tools/call` only when the translated REST result is `Code == 0`, per
  the shared wallet. `10001`/`20001`/`30001`/`90001` and unknown codes bill 0.
- The Zhihu Access Secret never appears in a tool result or log.
- Operator note: the Access Secret is an account-level credential. Don't rotate across personal
  accounts to farm free quota, and verify the Zhihu open-platform resale terms before any public
  offering.
