import '../../test/happydom'

import { afterEach, describe, expect, it, mock } from 'bun:test'

import { installDemoRuntime } from './demo'
import { handleProviderDemoRoute } from './demoProviders'

const DEMO_STORAGE_KEY = 'tavily-hikari-demo-mode'
const originalFetch = window.fetch
const originalEventSource = window.EventSource

function resetDemoRuntime(): void {
  window.fetch = originalFetch
  window.EventSource = originalEventSource
  delete document.documentElement.dataset.demoMode
  delete window.__tavilyHikariDemoFetch
  delete window.__tavilyHikariDemoEventSource
  delete window.__tavilyHikariDemoInstalled
  window.localStorage.removeItem(DEMO_STORAGE_KEY)
}

afterEach(() => {
  resetDemoRuntime()
})

function rpc(method: string, params?: Record<string, unknown>): RequestInit {
  return {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 18, method, params }),
  }
}

async function requiredRoute(
  path: string,
  method: string,
  init?: RequestInit,
  search = '',
): Promise<Response> {
  const response = await handleProviderDemoRoute(path, method, init, search)
  if (!response) throw new Error(`Expected a local demo response for ${path}`)
  return response
}

describe('Keenable and Zhihu demo routes (plan todo 18)', () => {
  it('answers every REST and MCP path locally with zero network requests', async () => {
    const networkSpy = mock(async () => new Response('unexpected network', { status: 599 }))
    window.fetch = networkSpy as unknown as typeof fetch
    window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
    installDemoRuntime()

    const responses = await Promise.all([
      window.fetch('/api/keenable/v1/search', {
        method: 'POST',
        body: JSON.stringify({ query: 'demo mode', mode: 'pro' }),
      }),
      window.fetch('/api/keenable/v1/fetch?url=https%3A%2F%2Fexample.com%2F1'),
      window.fetch('/api/zhihu/api/v1/content/zhihu_search?Query=demo+mode&Count=10'),
      window.fetch('/api/zhihu/api/v1/content/global_search?Query=demo+mode&Count=20'),
      window.fetch('/api/zhihu/api/v1/quota?APIIDs=zhihu_search'),
      window.fetch('/keenable/mcp', rpc('tools/list')),
      window.fetch('/zhihu/mcp', rpc('tools/list')),
    ])

    for (const response of responses) {
      expect(response.status).toBe(200)
      expect(response.headers.get('X-Tavily-Hikari-Demo')).toBe('true')
    }
    const search = await responses[0]!.json() as { query: string; usage: { credits_remaining: number } }
    expect(search.query).toBe('demo mode')
    expect(search.usage.credits_remaining).toBe(99_999)
    expect(responses[0]!.headers.get('x-ratelimit-limit')).toBe('10')
    const fetchFixture = await responses[1]!.json() as { url: string; usage: { sku: string } }
    expect(fetchFixture.url).toBe('https://example.com/1')
    expect(fetchFixture.usage.sku).toBe('fetch')
    const zhihuSearch = await responses[2]!.json() as { Code: number; Data: { Items: unknown[] } }
    expect(zhihuSearch.Code).toBe(0)
    expect(zhihuSearch.Data.Items).toHaveLength(1)
    const quota = await responses[4]!.json() as { Code: number; Data: Array<{ APIID: string }> }
    expect(quota.Code).toBe(0)
    expect(quota.Data.map((entry) => entry.APIID)).toEqual(['zhihu_search'])
    const keenableTools = await responses[5]!.json() as { result: { tools: Array<{ name: string }> } }
    expect(keenableTools.result.tools.map((tool) => tool.name)).toEqual([
      'search_web_pages',
      'fetch_page_content',
    ])
    const zhihuTools = await responses[6]!.json() as { result: { tools: Array<{ name: string }> } }
    expect(zhihuTools.result.tools.map((tool) => tool.name)).toEqual([
      'zhihu_search',
      'zhihu_global_search',
    ])
    expect(networkSpy).toHaveBeenCalledTimes(0)
  })

  it('mirrors Keenable validation, auth, credit, and rate-limit failures', async () => {
    const missing = await requiredRoute('/api/keenable/v1/search', 'POST', { body: '{}' })
    expect(missing.status).toBe(400)
    expect(await missing.json()).toMatchObject({ code: 'missing_parameter', param: 'query' })

    const unauthorized = await requiredRoute('/api/keenable/v1/search', 'POST', {
      headers: { 'X-API-Key': 'key-auth' },
      body: JSON.stringify({ query: 'demo' }),
    })
    expect(unauthorized.status).toBe(401)
    expect(await unauthorized.json()).toMatchObject({ code: 'invalid_api_key' })

    const exhausted = await requiredRoute('/api/keenable/v1/search', 'POST', {
      headers: { 'X-API-Key': 'key-exhausted' },
      body: JSON.stringify({ query: 'demo' }),
    })
    expect(exhausted.status).toBe(402)
    expect(await exhausted.json()).toMatchObject({ code: 'insufficient_credits' })

    const limited = await requiredRoute('/api/keenable/v1/fetch', 'GET', {
      headers: { 'X-API-Key': 'key-rate' },
    }, '?url=https%3A%2F%2Fexample.com%2F1')
    expect(limited.status).toBe(429)
    expect(limited.headers.get('Retry-After')).toBe('1')
    expect(await limited.json()).toMatchObject({ code: 'rate_limit_per_second', retryAfter: 1 })
  })

  it('keeps Zhihu failures in HTTP-200 Code envelopes and mirrors quota plans', async () => {
    for (const [token, code] of [['tok-auth', 20001], ['tok-rate', 30001], ['tok-internal', 90001]] as const) {
      const response = await requiredRoute('/api/zhihu/api/v1/content/zhihu_search', 'GET', {
        headers: { Authorization: `Bearer ${token}` },
      }, '?Query=demo')
      expect(response.status).toBe(200)
      expect(await response.json()).toMatchObject({ Code: code, Data: null })
    }

    const missing = await requiredRoute('/api/zhihu/api/v1/content/global_search', 'GET')
    expect(missing.status).toBe(200)
    expect(await missing.json()).toMatchObject({ Code: 10001, Data: null })

    const tight = await requiredRoute('/api/zhihu/api/v1/quota', 'GET', {
      headers: { Authorization: 'Bearer tok-quota-tight' },
    })
    expect(await tight.json()).toMatchObject({
      Code: 0,
      Data: [
        { APIID: 'global_search', TotalQuota: 20, RemainingQuota: 5 },
        { APIID: 'zhihu_search', TotalQuota: 10, RemainingQuota: 3 },
      ],
    })
    const exhausted = await requiredRoute('/api/zhihu/api/v1/quota', 'GET', {
      headers: { Authorization: 'Bearer tok-quota-zero' },
    })
    const snapshot = await exhausted.json() as { Data: Array<{ RemainingQuota: number }> }
    expect(snapshot.Data.every((entry) => entry.RemainingQuota === 0)).toBe(true)
  })

  it('returns native MCP tool results without changing existing provider handlers', async () => {
    const keenable = await requiredRoute('/keenable/mcp', 'POST', rpc('tools/call', {
      name: 'search_web_pages',
      arguments: { query: 'demo mode' },
    }))
    const keenableBody = await keenable.json() as {
      result: { structuredContent: { query: string }; _meta: { 'keenable/usage': { sku: string } } }
    }
    expect(keenableBody.result.structuredContent.query).toBe('demo mode')
    expect(keenableBody.result._meta['keenable/usage'].sku).toBe('search')

    const zhihu = await requiredRoute('/zhihu/mcp', 'POST', rpc('tools/call', {
      name: 'zhihu_global_search',
      arguments: { query: 'demo mode' },
    }))
    const zhihuBody = await zhihu.json() as { result: { structuredContent: { Code: number; status: number } } }
    expect(zhihuBody.result.structuredContent).toMatchObject({ Code: 0, status: 200 })

    const serpapi = await requiredRoute('/serpapi/mcp', 'POST', rpc('tools/list'))
    const serpapiBody = await serpapi.json() as { result: { tools: Array<{ name: string }> } }
    expect(serpapiBody.result.tools.map((tool) => tool.name)).toEqual([
      'search',
      'search_table',
      'search_dashboard',
    ])
  })

  it('declines paths outside the explicit provider routes', async () => {
    for (const path of [
      '/api/keenable/unknown',
      '/api/zhihu/unknown',
      '/keenable/mcp/extra',
      '/zhihu/mcp/extra',
      '/api/other/v1/search',
    ]) {
      expect(await handleProviderDemoRoute(path, 'GET')).toBeNull()
    }
  })
})
