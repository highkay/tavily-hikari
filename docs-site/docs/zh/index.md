# Tavily Hikari 文档

Tavily Hikari 是一个面向 Tavily 流量的 Rust + Axum 代理层。它负责多上游 key 轮转、SQLite
审计落盘、管理员与用户访问控制，并附带 React + Vite 运维控制台。

它不是一个单纯的反向代理，而是把 Tavily 接入拆成了四个稳定的对外交付面：

- 协议入口：同时提供 `/mcp` 和 `/api/tavily/*`，既能服务 MCP 客户端，也能服务 Cherry
  Studio 这类直接走 HTTP API 的客户端。
- 访问令牌层：终端用户只拿到 Hikari 自己签发的 `th-<id>-<secret>` token，不需要直接接触官方 Tavily key。
- 运维与鉴权层：管理员可通过 ForwardAuth 或内置管理员登录管理 key、查看日志、恢复或下线 key；普通用户可通过 Linux DO OAuth 登录并绑定自己的访问令牌。
- 审计与配额层：SQLite 持久化保存 key 状态、请求日志、配额/账本相关数据，方便追踪额度损耗、异常请求和用户侧用量。

你可以把这里当成 Tavily Hikari 的公开使用手册：先跑起来，再决定如何接入、如何部署、如何验收 UI。

## 它具体解决什么问题

- 统一入口：把 Tavily MCP 和 Tavily HTTP API 都收敛到同一个服务里。
- 密钥池调度：在多把 Tavily key 之间做短期亲和 + 全局最久未使用分配，并自动处理 `432 exhausted`。
- 安全隔离：客户端只看到 Hikari token，真实 Tavily key 只在管理员面可见。
- 可运营：提供用户控制台、管理员后台、Storybook 验收面和完整请求审计。
- 可部署：既能本地开发，也能走 Docker、Docker Compose、ForwardAuth 网关和高匿名代理链路。

## 搜索提供方

Tavily Hikari 把四个搜索平台收敛到同一个端点、同一个密钥池和同一个共享额度钱包。每个提供方保留自己的
请求格式、独立的上游密钥池和耗尽语义，而配额检查、审计日志与密钥轮换仍由 Hikari 统一处理。

- **Tavily** — `/mcp` 与 `/api/tavily/*`（search、extract、crawl、map、research、usage）
- **SerpApi** — `/serpapi/mcp` 与 `/api/serpapi/search`（同步搜索系列）
- **Exa** — `/exa/mcp`、`/api/exa/search`、`/api/exa/contents`、`/api/exa/answer`
- **Brave** — `/brave/mcp`、`/api/brave/res/v1/web/search`、`/api/brave/res/v1/news/search` 与
  `/api/brave/res/v1/llm/context`

四个平台都接受 Hikari 访问令牌（`th-<id>-<secret>`）并通过各自的鉴权槽位传入。端点细节见
[HTTP API 指南](/zh/http-api-guide)，提供方密钥引导与上游覆盖见[配置与访问](/zh/configuration-access)。

## 文档地图

1. 想先启动实例，看[快速开始](/zh/quick-start)。
2. 想理解环境变量、管理员登录和访问模型，看[配置与访问](/zh/configuration-access)。
3. 想接入 Cherry Studio 或其他 HTTP 客户端，看[HTTP API 指南](/zh/http-api-guide)。
4. 想部署到生产、反代或高匿名环境，看[部署与高匿名](/zh/deployment-anonymity)。
5. 想先处理常见报错、401/429/502 或持久化问题，看 [FAQ 与排障](/zh/faq)。
6. 想直接验收页面和组件状态，看 [Storybook](/zh/storybook.html)。

## 你会在这个项目里看到什么

- MCP 代理：给 Codex CLI、Claude Desktop、Cursor、VS Code 等 MCP 客户端使用。
- HTTP API 代理：给 Cherry Studio 等只支持 Tavily HTTP API 的客户端使用。
- 用户面：登录、查看自己的 token、检查近期请求和配额消耗。
- 管理员面：录入/恢复/下线上游 key、查看真实 key、检查审计日志和 key 健康状态。
- 文档与验收面：公开文档站负责说明使用方法，Storybook 负责核对页面状态和组件表现。
