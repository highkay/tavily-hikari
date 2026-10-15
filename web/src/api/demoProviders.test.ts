import '../../test/happydom'

import { afterEach, describe, expect, it } from 'bun:test'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import { installDemoRuntime } from './demo'
import {
  createDemoAnalysisProvidersSnapshot,
  createDemoProviderKeys,
  createDemoProviderLogs,
  createDemoUserProviderCounts,
} from './demoProviders'

const DEMO_STORAGE_KEY = 'tavily-hikari-demo-mode'
const originalFetch = window.fetch
const roots: Root[] = []

const demoSource = { keys: createDemoProviderKeys(), logs: createDemoProviderLogs() }

function resetDemoRuntime(): void {
  window.fetch = originalFetch
  delete document.documentElement.dataset.demoMode
  delete window.__tavilyHikariDemoFetch
  delete window.__tavilyHikariDemoEventSource
  delete window.__tavilyHikariDemoInstalled
  window.localStorage.removeItem(DEMO_STORAGE_KEY)
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    act(() => root.unmount())
  }
  resetDemoRuntime()
  document.body.innerHTML = ''
})

describe('createDemoAnalysisProvidersSnapshot (contract C1 demo parity)', () => {
  it('carries all three windows in one snapshot, sorted by requests', () => {
    const snapshot = createDemoAnalysisProvidersSnapshot(demoSource, 1_776_041_600)

    expect(snapshot.generated_at).toBe(1_776_041_600)
    expect(Object.keys(snapshot.windows).sort()).toEqual(['last24h', 'last30d', 'last7d'])
    for (const rows of Object.values(snapshot.windows)) {
      expect(rows.length).toBeGreaterThan(0)
      const requests = rows.map((row) => row.requests)
      expect([...requests].sort((left, right) => right - left)).toEqual(requests)
      for (const row of rows) {
        expect(row.provider.length).toBeGreaterThan(0)
        expect(row.success + row.errors + row.quota_exhausted).toBe(row.requests)
        expect(row.credits).toBeGreaterThanOrEqual(0)
        expect(row.credits).toBeLessThanOrEqual(row.success)
      }
    }
  })

  it('scales the same breakdown per window so 24h < 7d < 30d', () => {
    const { windows } = createDemoAnalysisProvidersSnapshot(demoSource, 1)
    const total = (rows: Array<{ requests: number }>) =>
      rows.reduce((sum, row) => sum + row.requests, 0)

    expect(total(windows.last24h)).toBeLessThan(total(windows.last7d))
    expect(total(windows.last7d)).toBeLessThan(total(windows.last30d))
    expect(total(windows.last24h)).toBe(760)
  })
})

describe('createDemoUserProviderCounts (contract C3 demo parity)', () => {
  it('groups only the console user own request rows by provider', () => {
    const rows = createDemoUserProviderCounts({
      logs: [
        { auth_token_id: 'dm01', result_status: 'success' },
        { auth_token_id: 'dm01', result_status: 'error' },
        { auth_token_id: 'dm01', provider: 'serpapi', result_status: 'success' },
        { auth_token_id: 'ops2', provider: 'brave', result_status: 'success' },
      ],
      tokens: [{ id: 'dm01' }],
    })

    // The other user's token rows stay out of the breakdown; legacy rows are tavily.
    expect(rows).toEqual([
      { provider: 'tavily', requests: 2, success: 1, errors: 1 },
      { provider: 'serpapi', requests: 1, success: 1, errors: 0 },
    ])
  })

  it('stays empty when none of the user tokens produced rows so the card hides', () => {
    expect(createDemoUserProviderCounts({ logs: demoSource.logs, tokens: [{ id: 'nobody' }] })).toEqual([])
  })

  it('covers the mixed-provider demo logs of the shipped demo user', () => {
    const rows = createDemoUserProviderCounts({
      logs: demoSource.logs,
      tokens: [{ id: 'dm01' }, { id: 'ops2' }],
    })

    expect(rows.length).toBeGreaterThan(1)
    for (const row of rows) {
      expect(row.requests).toBe(row.success + row.errors)
    }
  })
})

describe('demo runtime provider routes', () => {
  it('answers /api/analysis/providers and the user overview breakdown locally', async () => {
    window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
    installDemoRuntime()

    const analysisResponse = await window.fetch('/api/analysis/providers')
    expect(analysisResponse.status).toBe(200)
    const analysis = (await analysisResponse.json()) as {
      generated_at: number
      windows: Record<string, Array<{ provider: string; credits: number }>>
    }
    expect(analysis.generated_at).toBeGreaterThan(0)
    expect(Object.keys(analysis.windows).sort()).toEqual(['last24h', 'last30d', 'last7d'])
    expect(analysis.windows.last24h?.length ?? 0).toBeGreaterThan(0)

    const overviewResponse = await window.fetch('/api/user/dashboard/overview')
    const overview = (await overviewResponse.json()) as { providers?: Array<{ provider: string }> }
    expect(Array.isArray(overview.providers)).toBe(true)
    expect((overview.providers ?? []).length).toBeGreaterThan(0)
  })

  it('keeps the dashboard overview summary breakdown for the provider card', async () => {
    window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
    installDemoRuntime()

    const response = await window.fetch('/api/dashboard/overview')
    const overview = (await response.json()) as {
      summary?: { providers?: Array<{ provider: string }> }
      summaryWindows?: { today?: { providers?: unknown[] } }
    }

    // C2: the overview payload itself carries the breakdown the card renders.
    expect((overview.summary?.providers ?? []).length).toBeGreaterThan(0)
    expect((overview.summaryWindows?.today?.providers ?? []).length).toBeGreaterThan(0)
  })
})
