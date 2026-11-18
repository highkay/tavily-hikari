import { describe, expect, it } from 'bun:test'
import { Fragment, createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'

import {
  buildGuideContent,
  buildHikariCliInstallSnippet,
  guideSnippetToPlainText,
  resolveGuideSamples,
} from './guide'
import { resolveSetupGuide } from './guideSearch'
import type { GuideContent, GuideKey } from './runtime'
import { getGuideClientIconName } from '../lib/icons'

describe('Hikari CLI guide content', () => {
  it('renders configured installer and optional skills commands', () => {
    const guides = buildGuideContent('en', 'https://hikari.example.com', 'th-a1b2-secretsecret')
    const samples = resolveGuideSamples(guides.hikariCli)

    expect(guides.hikariCli.title).toBe('CLI + Agent Skills')
    expect(samples).toHaveLength(2)
    expect(samples[0].snippet).toContain('install-tvly-hikari.sh')
    expect(samples[0].snippet).toContain('--base-url "https://hikari.example.com"')
    expect(samples[0].snippet).toContain('--token "th-a1b2-secretsecret"')
    expect(samples[1].snippet).toBe('npx skills add https://github.com/IvanLi-CN/tavily-hikari --global')
  })

  it('converts highlighted snippets to copyable plain text', () => {
    const snippet = buildHikariCliInstallSnippet('http://127.0.0.1:58087', 'th-test-secretsecret')
    const plain = guideSnippetToPlainText(snippet)

    expect(plain).toContain('curl -fsSL')
    expect(plain).toContain('--base-url "http://127.0.0.1:58087"')
    expect(plain).toContain('--token "th-test-secretsecret"')
    expect(plain).not.toContain('<span')
  })
})

const PROVIDER_BASE_URL = 'https://hikari.example.com'
const PROVIDER_TOKEN = 'th-a1b2-secretsecret'

interface ProviderExpectation {
  key: 'serpapi' | 'exa' | 'brave' | 'keenable' | 'zhihu' | 'tavily'
  mcpPath: string
  httpPath: string
  httpAuthMarker: string
  boundary: { en: string, zh: string }
}

const PROVIDER_EXPECTATIONS: ProviderExpectation[] = [
  {
    key: 'serpapi',
    mcpPath: '/serpapi/mcp',
    httpPath: '/api/serpapi/search',
    httpAuthMarker: `api_key=${PROVIDER_TOKEN}`,
    boundary: { en: '/account', zh: '/account' },
  },
  {
    key: 'exa',
    mcpPath: '/exa/mcp',
    httpPath: '/api/exa/search',
    httpAuthMarker: `x-api-key: ${PROVIDER_TOKEN}`,
    boundary: { en: 'no streaming', zh: '流式' },
  },
  {
    key: 'brave',
    mcpPath: '/brave/mcp',
    httpPath: '/api/brave/res/v1/web/search',
    httpAuthMarker: `X-Subscription-Token: ${PROVIDER_TOKEN}`,
    boundary: { en: 'no URL extraction', zh: 'URL 抽取' },
  },
  {
    key: 'keenable',
    mcpPath: '/keenable/mcp',
    httpPath: '/api/keenable/v1/search',
    httpAuthMarker: `X-API-Key: ${PROVIDER_TOKEN}`,
    boundary: { en: '100,000 requests per month', zh: '100,000 个请求' },
  },
  {
    key: 'zhihu',
    mcpPath: '/zhihu/mcp',
    httpPath: '/api/zhihu/api/v1/content/zhihu_search',
    httpAuthMarker: `Authorization: Bearer ${PROVIDER_TOKEN}`,
    boundary: { en: 'Operator terms', zh: '运营条款' },
  },
  {
    key: 'tavily',
    mcpPath: '/mcp',
    httpPath: '/api/tavily/search',
    httpAuthMarker: `Authorization: Bearer ${PROVIDER_TOKEN}`,
    boundary: { en: 'own tabs', zh: '独立的标签页' },
  },
]

function readProviderGuide(
  guides: Record<GuideKey, GuideContent>,
  key: ProviderExpectation['key'],
): GuideContent | undefined {
  return (guides as Record<string, GuideContent | undefined>)[key]
}

function renderSteps(steps: GuideContent['steps']): string {
  return renderToStaticMarkup(createElement(Fragment, null, ...steps))
}

describe('provider setup guides', () => {
  it('renders every provider MCP + HTTP snippet with the correct auth slot in both languages', () => {
    for (const language of ['en', 'zh'] as const) {
      const guides = buildGuideContent(language, PROVIDER_BASE_URL, PROVIDER_TOKEN)
      for (const provider of PROVIDER_EXPECTATIONS) {
        const content = readProviderGuide(guides, provider.key)
        expect(content).toBeDefined()
        if (!content) continue

        const samples = resolveGuideSamples(content).map((sample) => ({
          language: sample.language,
          text: guideSnippetToPlainText(sample.snippet),
        }))

        const mcpSamples = samples.filter((sample) =>
          sample.text.includes(`${PROVIDER_BASE_URL}${provider.mcpPath}`),
        )
        expect(mcpSamples.length).toBeGreaterThanOrEqual(3)
        expect(
          mcpSamples.some((sample) => sample.language === 'toml' && sample.text.includes('bearer_token_env_var')),
        ).toBe(true)
        expect(
          mcpSamples.some((sample) =>
            sample.text.includes('claude mcp add-json') && sample.text.includes(`Bearer ${PROVIDER_TOKEN}`),
          ),
        ).toBe(true)
        expect(
          mcpSamples.some((sample) =>
            sample.language === 'json' && sample.text.includes('"servers"') && sample.text.includes(`Bearer ${PROVIDER_TOKEN}`),
          ),
        ).toBe(true)

        const httpSamples = samples.filter((sample) =>
          sample.text.includes(`${PROVIDER_BASE_URL}${provider.httpPath}`),
        )
        expect(httpSamples.length).toBeGreaterThanOrEqual(1)
        expect(httpSamples.some((sample) => sample.text.includes(provider.httpAuthMarker))).toBe(true)

        const boundaryText = renderSteps(content.steps)
        expect(boundaryText).toContain(provider.boundary[language])
      }
    }
  })

  it('documents the Brave Cherry Studio mapping and the OpenAI-style client boundary in both languages', () => {
    for (const language of ['en', 'zh'] as const) {
      const guides = buildGuideContent(language, PROVIDER_BASE_URL, PROVIDER_TOKEN)
      const brave = readProviderGuide(guides, 'brave')
      expect(brave).toBeDefined()
      if (!brave) continue
      const steps = renderSteps(brave.steps)
      expect(steps).toContain(`${PROVIDER_BASE_URL}/api/brave`)
      expect(steps).toContain('OpenAI')
    }
  })

  it('documents the Keenable and Zhihu contract surfaces without production upstream snippets', () => {
    for (const language of ['en', 'zh'] as const) {
      const guides = buildGuideContent(language, PROVIDER_BASE_URL, PROVIDER_TOKEN)
      const keenable = readProviderGuide(guides, 'keenable')
      const zhihu = readProviderGuide(guides, 'zhihu')
      expect(keenable).toBeDefined()
      expect(zhihu).toBeDefined()
      if (!keenable || !zhihu) continue

      const keenableSteps = renderSteps(keenable.steps)
      const keenableSamples = resolveGuideSamples(keenable)
        .map((sample) => guideSnippetToPlainText(sample.snippet))
        .join('\n')
      expect(keenableSteps).toContain(`${PROVIDER_BASE_URL}/api/keenable`)
      expect(keenableSteps).toContain('search_web_pages')
      expect(keenableSteps).toContain('fetch_page_content')
      expect(keenableSteps).toContain(language === 'en' ? '10 requests per second' : '每秒 10 个请求')
      expect(keenableSamples).toContain(`${PROVIDER_BASE_URL}/api/keenable/v1/fetch`)
      expect(keenableSamples).toContain(`X-API-Key: ${PROVIDER_TOKEN}`)
      expect(keenableSamples).not.toContain('api.keenable.ai')

      const zhihuSteps = renderSteps(zhihu.steps)
      const zhihuSamples = resolveGuideSamples(zhihu)
        .map((sample) => guideSnippetToPlainText(sample.snippet))
        .join('\n')
      expect(zhihuSteps).toContain(`${PROVIDER_BASE_URL}/api/zhihu`)
      expect(zhihuSteps).toContain('zhihu_search')
      expect(zhihuSteps).toContain('zhihu_global_search')
      expect(zhihuSteps).toContain('Count')
      expect(zhihuSteps).toContain('10')
      expect(zhihuSteps).toContain('20')
      expect(zhihuSteps).toContain('RemainingQuota')
      expect(zhihuSteps).toContain('Asia/Shanghai')
      expect(zhihuSamples).toContain(`${PROVIDER_BASE_URL}/api/zhihu/api/v1/content/global_search`)
      expect(zhihuSamples).toContain('Count=10')
      expect(zhihuSamples).toContain('Count=20')
      expect(zhihuSamples).not.toContain('developer.zhihu.com')
      expect(zhihuSteps).not.toContain('hot_list')
      expect(zhihuSteps).not.toContain('question_discovery')
      expect(zhihuSteps).not.toContain('zhida_openai')
    }
  })

  it('keeps the Tavily sections byte-stable', () => {
    const guides = buildGuideContent('en', PROVIDER_BASE_URL, PROVIDER_TOKEN)

    expect(guides.codex.snippet).toBe(
      [
        '<span class="hl-comment"># ~/.codex/config.toml</span>',
        '<span class="hl-key">experimental_use_rmcp_client</span> = <span class="hl-boolean">true</span>',
        '',
        '[<span class="hl-section">mcp_servers.tavily_hikari</span>]',
        `<span class="hl-key">url</span> = <span class="hl-string">"${PROVIDER_BASE_URL}/mcp"</span>`,
        '<span class="hl-key">bearer_token_env_var</span> = <span class="hl-string">"TAVILY_HIKARI_TOKEN"</span>',
      ].join('\n'),
    )

    const tavilyText = [guides.codex, guides.claude, guides.vscode, guides.other]
      .flatMap((content) => resolveGuideSamples(content))
      .map((sample) => guideSnippetToPlainText(sample.snippet))
      .join('\n')
    expect(tavilyText).toContain(`${PROVIDER_BASE_URL}/mcp`)
    expect(tavilyText).toContain(`${PROVIDER_BASE_URL}/api/tavily/search`)
    expect(tavilyText).not.toContain('/serpapi/mcp')
    expect(tavilyText).not.toContain('/exa/mcp')
    expect(tavilyText).not.toContain('/brave/mcp')
  })
})

describe('provider guide deep links', () => {
  it('resolves provider guide keys from the setup-guide query param', () => {
    expect(resolveSetupGuide('?guide=keenable')).toBe('keenable')
    expect(resolveSetupGuide('?guide=zhihu')).toBe('zhihu')
    expect(resolveSetupGuide('?guide=tavily')).toBe('tavily')
    expect(resolveSetupGuide('?guide=unknown')).toBe('codex')
  })
})

describe('provider guide tab icons', () => {
  it('resolves every provider tab to a distinct icon that is not the generic fallback', () => {
    const providerIcons = PROVIDER_EXPECTATIONS.map((provider) => getGuideClientIconName(provider.key))

    expect(providerIcons).toHaveLength(PROVIDER_EXPECTATIONS.length)
    expect(new Set(providerIcons).size).toBe(PROVIDER_EXPECTATIONS.length)
    expect(providerIcons).not.toContain(getGuideClientIconName('other'))
  })
})