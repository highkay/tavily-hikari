import type { ReactNode } from 'react'

export type ProviderGuideKey = 'serpapi' | 'exa' | 'brave' | 'keenable' | 'zhihu' | 'tavily'

type ProviderGuideLanguage = 'toml' | 'json' | 'bash'

export interface ProviderGuideSample {
  title: string
  language?: ProviderGuideLanguage
  snippet: string
}

export interface ProviderGuideContent {
  title: string
  steps: ReactNode[]
  samples: ProviderGuideSample[]
}

/**
 * Per-provider setup guide content (plan todo 24). Grounded in the committed
 * backend surfaces:
 * - `src/server/provider_facade.rs:163-167` MCP ingresses `/serpapi/mcp`,
 *   `/exa/mcp`, `/brave/mcp` (all accept `Authorization: Bearer th-...`).
 * - `src/server/provider_facade.rs:88-159` REST routes: SerpApi
 *   `GET|POST /api/serpapi/search` (client slot `api_key`), Exa
 *   `POST /api/exa/{search,contents,answer}` (client slot `x-api-key`), Brave
 *   `GET /api/brave/res/v1/{web/search,news/search,llm/context}` (client slot
 *   `X-Subscription-Token`); slot names pinned by `src/providers/spec.rs`.
 * - Tool catalogs: `src/server/provider_mcp_serpapi.rs:30`,
 *   `provider_mcp_exa.rs:211-233`, `provider_mcp_brave.rs:178-205`.
 * - Keenable and Zhihu routes, authentication slots, tool catalogs, and limits are
 *   pinned by `docs/specs/keenable-zhihu-providers/SPEC.md`.
 * - Tavily itself: `src/server/serve.rs:566` MCP ingress `/mcp` (passthrough;
 *   tool names normalized as `tavily-{search,extract,crawl,map,research}` in
 *   `src/analysis.rs:879-883`) plus `src/server/serve.rs:353-362` REST routes
 *   `/api/tavily/{search,extract,crawl,map,research,usage}`.
 * Boundaries: Exa streaming is a typed 400 before any upstream call
 * (`provider_facade.rs:435-445`); Brave has no URL-extraction route; SerpApi
 * mirrors the search family only; the `tvly-hikari` CLI wrapper targets the
 * Tavily REST family only.
 */
export function buildProviderGuideContent(
  language: 'en' | 'zh',
  baseUrl: string,
  prettyToken: string,
): Record<ProviderGuideKey, ProviderGuideContent> {
  const isEnglish = language === 'en'
  return {
    serpapi: {
      title: 'SerpApi',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/serpapi/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; this is the hosted SerpApi MCP passthrough.</>,
            <>The façade advertises <code>search</code>, <code>search_table</code>, and <code>search_dashboard</code>.</>,
            <>HTTP clients can call <code>GET {baseUrl}/api/serpapi/search?q=...&api_key=th-...</code>, sending the Hikari token in the <code>api_key</code> query slot; a JSON <code>POST</code> convenience is also accepted.</>,
            <>Boundary: only the SerpApi search family is mirrored here — <code>engine</code> and other search parameters pass through as-is, and plan/quota state syncs from SerpApi&apos;s <code>/account</code> surface.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/serpapi/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；这是 SerpApi 托管 MCP 的透传入口。</>,
            <>该入口会通告 <code>search</code>、<code>search_table</code>、<code>search_dashboard</code> 三个工具。</>,
            <>HTTP 客户端可调用 <code>GET {baseUrl}/api/serpapi/search?q=...&api_key=th-...</code>，把 Hikari token 放进 <code>api_key</code> 查询参数；同时也接受 JSON <code>POST</code> 便捷写法。</>,
            <>边界：这里只镜像 SerpApi 搜索能力，<code>engine</code> 等搜索参数原样透传；套餐与额度状态从 SerpApi 的 <code>/account</code> 同步。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('serpapi_hikari', baseUrl, '/serpapi/mcp', 'SERPAPI_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('serpapi-hikari', baseUrl, '/serpapi/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('serpapi-hikari', baseUrl, '/serpapi/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: GET /api/serpapi/search' : '示例：GET /api/serpapi/search',
          language: 'bash',
          snippet: buildSerpapiHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
    exa: {
      title: 'Exa',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/exa/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; Hikari runs this facade locally.</>,
            <>The façade advertises <code>web_search_exa</code>, <code>web_fetch_exa</code>, and <code>exa_answer</code>.</>,
            <>HTTP clients can call <code>POST {baseUrl}/api/exa/search</code> (plus <code>/contents</code> and <code>/answer</code>) with a JSON body and the Hikari token in the <code>x-api-key</code> header.</>,
            <>Boundary: no streaming — omit <code>stream</code> or set it to <code>false</code>; a truthy <code>stream</code> value is rejected with a typed 400. Only search, contents, and answer are exposed.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/exa/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；该入口由 Hikari 本地提供。</>,
            <>该入口会通告 <code>web_search_exa</code>、<code>web_fetch_exa</code>、<code>exa_answer</code> 三个工具。</>,
            <>HTTP 客户端可调用 <code>POST {baseUrl}/api/exa/search</code>（以及 <code>/contents</code>、<code>/answer</code>），使用 JSON 请求体，并把 Hikari token 放进 <code>x-api-key</code> 请求头。</>,
            <>边界：不支持流式输出——请省略 <code>stream</code> 或设为 <code>false</code>；传入真值会收到类型化 400。仅开放 search、contents、answer 三类能力。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('exa_hikari', baseUrl, '/exa/mcp', 'EXA_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('exa-hikari', baseUrl, '/exa/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('exa-hikari', baseUrl, '/exa/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: POST /api/exa/search' : '示例：POST /api/exa/search',
          language: 'bash',
          snippet: buildExaHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
    brave: {
      title: 'Brave',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/brave/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; Hikari runs this facade locally.</>,
            <>The façade advertises <code>brave_web_search</code>, <code>brave_news_search</code>, and <code>brave_llm_context</code>.</>,
            <>HTTP clients can call <code>GET {baseUrl}/api/brave/res/v1/web/search?q=...</code> with the Hikari token in the <code>X-Subscription-Token</code> header; the sibling native paths <code>/news/search</code> and <code>/llm/context</code> are mirrored too.</>,
            <>Cherry Studio: choose the <strong>Brave Search</strong> provider and set its base URL to <code>{baseUrl}/api/brave</code>; the native <code>/res/v1</code> paths are mirrored, so base-URL-override clients work unchanged.</>,
            <>Boundary: web search, news search, and LLM context only — there is no URL extraction endpoint. Generic OpenAI-style chat clients are not applicable; use the MCP facade or the native Brave paths.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/brave/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；该入口由 Hikari 本地提供。</>,
            <>该入口会通告 <code>brave_web_search</code>、<code>brave_news_search</code>、<code>brave_llm_context</code> 三个工具。</>,
            <>HTTP 客户端可调用 <code>GET {baseUrl}/api/brave/res/v1/web/search?q=...</code>，把 Hikari token 放进 <code>X-Subscription-Token</code> 请求头；同样镜像了 <code>/news/search</code> 与 <code>/llm/context</code> 原生路径。</>,
            <>Cherry Studio：选择 <strong>Brave Search</strong> 服务商，将 Base URL 设为 <code>{baseUrl}/api/brave</code>；原生 <code>/res/v1</code> 路径已镜像，支持 Base URL 覆盖的客户端无需额外映射。</>,
            <>边界：仅镜像网页搜索、新闻搜索与 LLM 上下文，没有 URL 抽取端点。通用 OpenAI 风格聊天客户端不适用；请使用 MCP 入口或 Brave 原生路径。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('brave_hikari', baseUrl, '/brave/mcp', 'BRAVE_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('brave-hikari', baseUrl, '/brave/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('brave-hikari', baseUrl, '/brave/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: GET /api/brave/res/v1/web/search' : '示例：GET /api/brave/res/v1/web/search',
          language: 'bash',
          snippet: buildBraveHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
    keenable: {
      title: 'Keenable',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/keenable/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; Hikari passes through to Keenable&apos;s hosted MCP.</>,
            <>The façade advertises <code>search_web_pages</code> and <code>fetch_page_content</code>.</>,
            <>The REST base is <code>{baseUrl}/api/keenable</code>. Call <code>POST /v1/search</code> or <code>GET /v1/fetch?url=...</code> with the Hikari token in <code>X-API-Key</code>; <code>Authorization: Bearer {prettyToken}</code> is also accepted.</>,
            <>Limits: 10 requests per second per organization, shared across keys and search/fetch. The free allowance is 100,000 requests per month and resets monthly.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/keenable/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；Hikari 会透传到 Keenable 托管 MCP。</>,
            <>该入口会通告 <code>search_web_pages</code> 与 <code>fetch_page_content</code> 两个工具。</>,
            <>REST 基础地址是 <code>{baseUrl}/api/keenable</code>。调用 <code>POST /v1/search</code> 或 <code>GET /v1/fetch?url=...</code> 时，把 Hikari token 放进 <code>X-API-Key</code>；也接受 <code>Authorization: Bearer {prettyToken}</code>。</>,
            <>限制：每个组织每秒 10 个请求，所有 key 以及 search/fetch 共享该额度。免费额度为每月 100,000 个请求，按月重置。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('keenable_hikari', baseUrl, '/keenable/mcp', 'KEENABLE_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('keenable-hikari', baseUrl, '/keenable/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('keenable-hikari', baseUrl, '/keenable/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: POST /api/keenable/v1/search' : '示例：POST /api/keenable/v1/search',
          language: 'bash',
          snippet: buildKeenableSearchHttpSnippet(baseUrl, prettyToken),
        },
        {
          title: isEnglish ? 'Example: GET /api/keenable/v1/fetch' : '示例：GET /api/keenable/v1/fetch',
          language: 'bash',
          snippet: buildKeenableFetchHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
    zhihu: {
      title: 'Zhihu',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/zhihu/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; Hikari runs this facade locally.</>,
            <>The façade advertises exactly <code>zhihu_search</code> and <code>zhihu_global_search</code>; no other Zhihu tools are exposed.</>,
            <>The REST base is <code>{baseUrl}/api/zhihu</code>. Call <code>GET /api/v1/content/zhihu_search</code> or <code>GET /api/v1/content/global_search</code> with <code>Authorization: Bearer {prettyToken}</code>. Hikari adds the account Access Secret and <code>X-Request-Timestamp</code> only to the upstream request.</>,
            <><code>Query</code> is required. <code>Count</code> is capped at 10 for <code>zhihu_search</code> and 20 for <code>global_search</code>; larger values are rejected locally. <code>SortBy</code>, <code>SearchDB</code>, and <code>Filter</code> pass through unchanged.</>,
            <>Quota is tracked per natural day. Hikari reads <code>RemainingQuota</code> from <code>/api/v1/quota</code> and restores an exhausted key at the next Asia/Shanghai midnight.</>,
            <>Operator terms: the Access Secret is account-level. Do not rotate or farm multiple accounts, and verify Zhihu&apos;s resale terms before offering this publicly.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/zhihu/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；该入口由 Hikari 本地提供。</>,
            <>该入口只通告 <code>zhihu_search</code> 与 <code>zhihu_global_search</code> 两个工具，不提供其他知乎工具。</>,
            <>REST 基础地址是 <code>{baseUrl}/api/zhihu</code>。调用 <code>GET /api/v1/content/zhihu_search</code> 或 <code>GET /api/v1/content/global_search</code> 时，携带 <code>Authorization: Bearer {prettyToken}</code>。Hikari 只在发往上游时添加账号级 Access Secret 和 <code>X-Request-Timestamp</code>。</>,
            <><code>Query</code> 为必填项。<code>zhihu_search</code> 的 <code>Count</code> 上限为 10，<code>global_search</code> 为 20；超过上限会在本地拒绝。<code>SortBy</code>、<code>SearchDB</code> 与 <code>Filter</code> 原样透传。</>,
            <>额度按自然日统计。Hikari 从 <code>/api/v1/quota</code> 读取 <code>RemainingQuota</code>，并在下一个 Asia/Shanghai 午夜恢复耗尽的 key。</>,
            <>运营条款：Access Secret 属于账号级凭证。不要轮换或批量经营多个账号；公开提供此服务前，请核实知乎的转售条款。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('zhihu_hikari', baseUrl, '/zhihu/mcp', 'ZHIHU_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('zhihu-hikari', baseUrl, '/zhihu/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('zhihu-hikari', baseUrl, '/zhihu/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: GET /api/zhihu/api/v1/content/zhihu_search' : '示例：GET /api/zhihu/api/v1/content/zhihu_search',
          language: 'bash',
          snippet: buildZhihuSearchHttpSnippet(baseUrl, prettyToken),
        },
        {
          title: isEnglish ? 'Example: GET /api/zhihu/api/v1/content/global_search' : '示例：GET /api/zhihu/api/v1/content/global_search',
          language: 'bash',
          snippet: buildZhihuGlobalSearchHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
    tavily: {
      title: 'Tavily',
      steps: isEnglish
        ? [
            <>Point any HTTP MCP client at <code>{baseUrl}/mcp</code> with <code>Authorization: Bearer {prettyToken}</code>; this is the hosted Tavily MCP passthrough.</>,
            <>The façade advertises <code>tavily-search</code>, <code>tavily-extract</code>, <code>tavily-crawl</code>, <code>tavily-map</code>, and <code>tavily-research</code>.</>,
            <>HTTP clients can call <code>POST {baseUrl}/api/tavily/search</code>; the REST family also mirrors <code>/extract</code>, <code>/crawl</code>, <code>/map</code>, <code>/research</code>, and <code>GET /usage</code>. Send the Hikari token as <code>Authorization: Bearer {prettyToken}</code>, or as the JSON field <code>api_key</code> when headers are unavailable.</>,
            <>The <code>tvly-hikari</code> CLI wrapper is Tavily-only by design: it injects <code>{baseUrl}/api/tavily</code> and the Hikari token for <code>search/extract/crawl/map/research</code> — see the <strong>CLI + Skills</strong> tab.</>,
            <>Boundary: other providers have their own tabs — SerpApi, Exa, and Brave each expose a separate MCP ingress and REST family.</>,
          ]
        : [
            <>将任意 HTTP MCP 客户端指向 <code>{baseUrl}/mcp</code>，并携带 <code>Authorization: Bearer {prettyToken}</code>；这是 Tavily 托管 MCP 的透传入口。</>,
            <>该入口会通告 <code>tavily-search</code>、<code>tavily-extract</code>、<code>tavily-crawl</code>、<code>tavily-map</code>、<code>tavily-research</code> 五个工具。</>,
            <>HTTP 客户端可调用 <code>POST {baseUrl}/api/tavily/search</code>；REST 家族同样镜像 <code>/extract</code>、<code>/crawl</code>、<code>/map</code>、<code>/research</code> 与 <code>GET /usage</code>。推荐用 <code>Authorization: Bearer {prettyToken}</code> 携带 Hikari token；无法自定义 Header 时，可写入 JSON 请求体字段 <code>api_key</code>。</>,
            <><code>tvly-hikari</code> CLI wrapper 按设计只服务 Tavily：它会注入 <code>{baseUrl}/api/tavily</code> 与 Hikari token，覆盖 <code>search/extract/crawl/map/research</code> 子命令，详见 <strong>CLI + Skills</strong> 标签页。</>,
            <>边界：其他服务商有各自独立的标签页——SerpApi、Exa、Brave 分别提供自己的 MCP 入口与 REST 路由。</>,
          ],
      samples: [
        {
          title: isEnglish ? 'Example: ~/.codex/config.toml' : '示例：~/.codex/config.toml',
          language: 'toml',
          snippet: buildProviderCodexSnippet('tavily_hikari', baseUrl, '/mcp', 'TAVILY_HIKARI_TOKEN'),
        },
        {
          title: isEnglish ? 'Example: claude mcp add-json' : '示例：claude mcp add-json',
          language: 'bash',
          snippet: buildProviderClaudeSnippet('tavily-hikari', baseUrl, '/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: VS Code / generic mcp.json' : '示例：VS Code / 通用 mcp.json',
          language: 'json',
          snippet: buildProviderVscodeSnippet('tavily-hikari', baseUrl, '/mcp', prettyToken),
        },
        {
          title: isEnglish ? 'Example: POST /api/tavily/search' : '示例：POST /api/tavily/search',
          language: 'bash',
          snippet: buildTavilyHttpSnippet(baseUrl, prettyToken),
        },
      ],
    },
  }
}

export function buildProviderCodexSnippet(
  serverName: string,
  baseUrl: string,
  mcpPath: string,
  tokenEnvVar: string,
): string {
  return [
    '<span class="hl-comment"># ~/.codex/config.toml</span>',
    '<span class="hl-key">experimental_use_rmcp_client</span> = <span class="hl-boolean">true</span>',
    '',
    `[<span class="hl-section">mcp_servers.${serverName}</span>]`,
    `<span class="hl-key">url</span> = <span class="hl-string">"${baseUrl}${mcpPath}"</span>`,
    `<span class="hl-key">bearer_token_env_var</span> = <span class="hl-string">"${tokenEnvVar}"</span>`,
  ].join('\n')
}

export function buildProviderClaudeSnippet(
  serverName: string,
  baseUrl: string,
  mcpPath: string,
  prettyToken: string,
): string {
  return [
    '<span class="hl-comment"># claude mcp add-json</span>',
    `claude mcp add-json ${serverName} '{`,
    '  <span class="hl-key">"type"</span>: <span class="hl-string">"http"</span>,',
    `  <span class="hl-key">"url"</span>: <span class="hl-string">"${baseUrl}${mcpPath}"</span>,`,
    '  <span class="hl-key">"headers"</span>: {',
    `    <span class="hl-key">"Authorization"</span>: <span class="hl-string">"Bearer ${prettyToken}"</span>`,
    '  }',
    "}'",
  ].join('\n')
}

export function buildProviderVscodeSnippet(
  serverName: string,
  baseUrl: string,
  mcpPath: string,
  prettyToken: string,
): string {
  return [
    '{',
    '  <span class="hl-key">"servers"</span>: {',
    `    <span class="hl-key">"${serverName}"</span>: {`,
    '      <span class="hl-key">"type"</span>: <span class="hl-string">"http"</span>,',
    `      <span class="hl-key">"url"</span>: <span class="hl-string">"${baseUrl}${mcpPath}"</span>,`,
    '      <span class="hl-key">"headers"</span>: {',
    `        <span class="hl-key">"Authorization"</span>: <span class="hl-string">"Bearer ${prettyToken}"</span>`,
    '      }',
    '    }',
    '  }',
    '}',
  ].join('\n')
}

export function buildSerpapiHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl "${baseUrl}/api/serpapi/search?q=coffee&engine=google&api_key=${prettyToken}"`
}

export function buildExaHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -X POST "${baseUrl}/api/exa/search" \\
  -H "Content-Type: application/json" \\
  -H "x-api-key: ${prettyToken}" \\
  -d '{
    "query": "latest AI agent news",
    "numResults": 5
  }'`
}

export function buildBraveHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl "${baseUrl}/api/brave/res/v1/web/search?q=coffee&count=5" \\
  -H "X-Subscription-Token: ${prettyToken}"`
}

export function buildKeenableSearchHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -X POST "${baseUrl}/api/keenable/v1/search" \\
  -H "Content-Type: application/json" \\
  -H "X-API-Key: ${prettyToken}" \\
  -d '{
    "query": "latest AI agent news",
    "max_results": 5
  }'`
}

export function buildKeenableFetchHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -G "${baseUrl}/api/keenable/v1/fetch" \\
  -H "X-API-Key: ${prettyToken}" \\
  --data-urlencode "url=https://example.com"`
}

export function buildZhihuSearchHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -G "${baseUrl}/api/zhihu/api/v1/content/zhihu_search" \\
  -H "Authorization: Bearer ${prettyToken}" \\
  --data-urlencode "Query=AI agents" \\
  --data-urlencode "Count=10"`
}

export function buildZhihuGlobalSearchHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -G "${baseUrl}/api/zhihu/api/v1/content/global_search" \\
  -H "Authorization: Bearer ${prettyToken}" \\
  --data-urlencode "Query=AI agents" \\
  --data-urlencode "Count=20" \\
  --data-urlencode "SearchDB=all"`
}

export function buildTavilyHttpSnippet(baseUrl: string, prettyToken: string): string {
  return `curl -X POST "${baseUrl}/api/tavily/search" \\
  -H "Content-Type: application/json" \\
  -H "Authorization: Bearer ${prettyToken}" \\
  -d '{
    "query": "latest AI agent news",
    "max_results": 5
  }'`
}