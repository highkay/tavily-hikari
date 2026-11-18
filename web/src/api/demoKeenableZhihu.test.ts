import { describe, expect, it } from 'bun:test'

import {
  KEENABLE_MCP_TOOLS,
  demoKeenableFetchFixture,
  demoKeenableInsufficientCreditsFixture,
  demoKeenableInvalidKeyFixture,
  demoKeenableMissingParameterFixture,
  demoKeenableRateLimitHeaders,
  demoKeenableRateLimitedFixture,
  demoKeenableSearchFixture,
} from './demoKeenable'
import {
  ZHIHU_MCP_TOOLS,
  demoZhihuErrorFixture,
  demoZhihuGlobalSearchFixture,
  demoZhihuQuotaFixture,
  demoZhihuSearchFixture,
} from './demoZhihu'
import {
  createDemoProviderKeys,
  createDemoProviderLogs,
  createDemoProviderRequestKindOptions,
  filterDemoProviderKeys,
} from './demoProviders'

describe('keenable demo fixtures (plan todo 15, no network)', () => {
  it('carries the native search body with the passive credit balance', () => {
    const fixture = demoKeenableSearchFixture('demo mode', 'pro')
    expect(fixture.query).toBe('demo mode')
    expect(fixture.mode).toBe('pro')
    expect(fixture.results[0]?.url).toBe('https://example.com/1')
    expect(fixture.usage.sku).toBe('search')
    expect(fixture.usage.credits_remaining).toBeLessThan(100_000)
  })

  it('carries the native fetch body whose length matches total_chars', () => {
    const fixture = demoKeenableFetchFixture()
    expect(fixture.truncated).toBe(false)
    expect(fixture.total_chars).toBe(fixture.content.length)
    expect(fixture.usage.sku).toBe('fetch')
  })

  it('exposes the passive X-RateLimit headers as strings', () => {
    const headers = demoKeenableRateLimitHeaders(98_420)
    expect(headers['x-ratelimit-remaining']).toBe('98420')
    expect(Number(headers['x-ratelimit-limit'])).toBeGreaterThan(0)
  })

  it('mirrors the closed error envelope for each scripted arm', () => {
    expect(demoKeenableInvalidKeyFixture().code).toBe('invalid_api_key')
    expect(demoKeenableInsufficientCreditsFixture().code).toBe('insufficient_credits')
    expect(demoKeenableRateLimitedFixture().retry_after_seconds).toBe(1)
    expect(demoKeenableMissingParameterFixture().param).toBe('query')
  })

  it('publishes the two hosted MCP tools', () => {
    expect(KEENABLE_MCP_TOOLS.map((tool) => tool.name)).toEqual([
      'search_web_pages',
      'fetch_page_content',
    ])
  })
})

describe('zhihu demo fixtures (plan todo 15, no network)', () => {
  it('wraps both search endpoints in the native Code=0 envelope', () => {
    const local = demoZhihuSearchFixture('demo mode')
    const global = demoZhihuGlobalSearchFixture('demo mode')
    expect(local.Code).toBe(0)
    expect(global.Code).toBe(0)
    expect(local.Data.Items[0]?.Url).toBe('https://zhuanlan.zhihu.com/p/mock')
    expect(local.Data.SearchHashId).not.toBe(global.Data.SearchHashId)
  })

  it('mirrors the tight 3/10 and 5/20 daily quota plan', () => {
    const tight = demoZhihuQuotaFixture('tight')
    const byId = new Map(tight.Data.map((entry) => [entry.APIID, entry]))
    expect(byId.get('zhihu_search')).toMatchObject({ TotalQuota: 10, RemainingQuota: 3 })
    expect(byId.get('global_search')).toMatchObject({ TotalQuota: 20, RemainingQuota: 5 })
    const exhausted = demoZhihuQuotaFixture('exhausted')
    expect(exhausted.Data.every((entry) => entry.RemainingQuota === 0)).toBe(true)
  })

  it('carries the typed HTTP-200 Code failures', () => {
    expect(demoZhihuErrorFixture(20001).Message).toBe('Authorization failed')
    expect(demoZhihuErrorFixture(30001).Code).toBe(30001)
    expect(demoZhihuErrorFixture(90001).Code).toBe(90001)
    expect(demoZhihuErrorFixture(10001).Data).toBeNull()
  })

  it('publishes the two local-facade tools with their Count caps', () => {
    expect(ZHIHU_MCP_TOOLS.map((tool) => tool.name)).toEqual([
      'zhihu_search',
      'zhihu_global_search',
    ])
    const schema = (name: string) =>
      ZHIHU_MCP_TOOLS.find((tool) => tool.name === name)?.inputSchema as {
        properties: { count?: { maximum?: number } }
      }
    expect(schema('zhihu_search').properties.count?.maximum).toBe(10)
    expect(schema('zhihu_global_search').properties.count?.maximum).toBe(20)
  })
})

describe('mixed-provider demo factories (plan todo 15)', () => {
  it('adds one demo key per new provider with its quota posture', () => {
    const keys = createDemoProviderKeys()
    const keenable = keys.find((key) => key.provider === 'keenable')
    const zhihu = keys.find((key) => key.provider === 'zhihu')
    expect(keenable).toMatchObject({ quota_limit: null, quota_source: 'passive' })
    expect(zhihu).toMatchObject({ quota_limit: 5000, quota_source: 'live' })
    expect(Number(zhihu?.quota_renews_at)).toBeGreaterThan(Date.now() / 1000)
  })

  it('filters demo keys by provider and coalesces legacy rows to Tavily', () => {
    const keys = [...createDemoProviderKeys(), { id: 'legacy', status: 'active' }]
    expect(filterDemoProviderKeys(keys, 'zhihu').map((key) => key.id)).toEqual(['Zh10'])
    expect(filterDemoProviderKeys(keys, 'keenable').map((key) => key.id)).toEqual(['Kn09'])
    expect(filterDemoProviderKeys(keys, 'tavily').map((key) => key.id)).toEqual(['legacy'])
    expect(filterDemoProviderKeys(keys, null)).toHaveLength(keys.length)
  })

  it('adds one demo log per new kind and keeps the catalog at 24 kinds', () => {
    const logs = createDemoProviderLogs()
    expect(logs).toHaveLength(24)
    expect(new Set(logs.map((log) => log.provider))).toEqual(
      new Set(['serpapi', 'exa', 'brave', 'keenable', 'zhihu']),
    )
    const kinds = createDemoProviderRequestKindOptions()
    expect(kinds).toHaveLength(24)
    for (const kind of [
      'api:keenable/search',
      'api:keenable/fetch',
      'mcp:keenable/search_web_pages',
      'mcp:keenable/fetch_page_content',
      'api:zhihu/zhihu_search',
      'api:zhihu/global_search',
      'mcp:zhihu/zhihu_search',
      'mcp:zhihu/global_search',
    ]) {
      expect(kinds.some((option) => option.key === kind)).toBe(true)
    }
  })
})
