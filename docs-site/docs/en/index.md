# Tavily Hikari Docs

Tavily Hikari is a Rust + Axum proxy for Tavily traffic. It rotates multiple upstream keys, keeps
full SQLite-backed audit logs, supports admin and end-user access flows, and ships with a React +
Vite operator console.

It is not just a reverse proxy. Tavily Hikari packages Tavily access into four stable surfaces:

- Protocol entrypoints: it serves both `/mcp` and `/api/tavily/*`, so it can sit in front of MCP
  clients and plain Tavily HTTP API clients.
- Access-token layer: end users receive Hikari-issued `th-<id>-<secret>` tokens instead of seeing
  raw Tavily API keys.
- Operator and auth layer: admins can manage keys through ForwardAuth or the built-in admin login,
  while end users can sign in with Linux DO OAuth and reuse their bound token.
- Audit and quota layer: SQLite persists key state, request logs, and quota-related records for
  troubleshooting, accounting, and operational review.

Treat this site as the public operator guide: get the service running, choose an access model,
integrate clients, then review UI states when needed.

## What problems it solves

- One service fronting both Tavily MCP and Tavily HTTP API traffic.
- Multi-key scheduling with short-lived affinity, global least-recently-used balancing, and
  automatic handling for `432 exhausted`.
- Secret isolation: clients use Hikari tokens while raw Tavily keys stay admin-only.
- Operability: user console, admin console, Storybook review surface, and full request audit data.
- Deployability: local dev, Docker, Docker Compose, ForwardAuth gateways, and high-anonymity
  topologies are all first-class use cases.

## Search providers

Tavily Hikari fronts four search providers behind one endpoint, one key pool, and one shared credit
wallet. Each provider keeps its native request shape, its own upstream key pool, and its own
exhaustion semantics, while quota checks, audit logging, and key rotation stay inside Hikari.

- **Tavily** — `/mcp` and `/api/tavily/*` (search, extract, crawl, map, research, usage)
- **SerpApi** — `/serpapi/mcp` and `/api/serpapi/search` (synchronous search family)
- **Exa** — `/exa/mcp`, `/api/exa/search`, `/api/exa/contents`, `/api/exa/answer`
- **Brave** — `/brave/mcp`, `/api/brave/res/v1/web/search`, `/api/brave/res/v1/news/search`, and
  `/api/brave/res/v1/llm/context`

All four accept Hikari access tokens (`th-<id>-<secret>`) in their native auth slots. See the
[HTTP API Guide](/http-api-guide) for endpoint details and
[Configuration & Access](/configuration-access) for provider key bootstrap and upstream overrides.

## Documentation map

1. Start with [Quick Start](/quick-start) when you want a running instance fast.
2. Open [Configuration & Access](/configuration-access) for environment variables, admin login, and
   access patterns.
3. Use [HTTP API Guide](/http-api-guide) when integrating Cherry Studio or other HTTP clients.
4. Read [Deployment & Anonymity](/deployment-anonymity) for production, proxying, and
   high-anonymity notes.
5. Open [FAQ & Troubleshooting](/faq) when you need answers for 401, 429, 502, persistence, or
   admin-access problems.
6. Visit [Storybook](/storybook.html) for UI review instead of prose.

## What you will find in this project

- MCP proxying for clients such as Codex CLI, Claude Desktop, Cursor, and VS Code.
- HTTP API proxying for clients such as Cherry Studio that speak Tavily over REST instead of MCP.
- User-facing flows for login, token lookup, recent request inspection, and quota visibility.
- Admin-facing flows for key registration, recovery, disablement, secret reveal, and audit review.
- Documentation and review surfaces where the docs site explains usage and Storybook verifies UI
  states and component behavior.
