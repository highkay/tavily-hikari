import { afterEach, describe, expect, it, mock } from 'bun:test'

import {
  ANALYSIS_PROVIDER_WINDOW_KEYS,
  fetchAdminAnalysisProviders,
  parseAnalysisProvidersSnapshot,
} from './adminAnalysisProviders'

const originalFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = originalFetch
})

/** The verified C1 wire shape (`src/server/dto.rs`): snake_case, three windows. */
const WIRE_SNAPSHOT = {
  generated_at: 1_776_041_600,
  windows: {
    last24h: [
      { provider: 'tavily', requests: 1607, success: 1500, errors: 100, quota_exhausted: 7, credits: 1498 },
      { provider: 'serpapi', requests: 41, success: 39, errors: 2, quota_exhausted: 0, credits: 39 },
    ],
    last7d: [
      { provider: 'tavily', requests: 9842, success: 9310, errors: 486, quota_exhausted: 46, credits: 9288 },
    ],
    last30d: [
      { provider: 'brave', requests: 1980, success: 1871, errors: 96, quota_exhausted: 13, credits: 1862 },
    ],
  },
}

describe('parseAnalysisProvidersSnapshot (contract C1)', () => {
  it('parses the snake_case wire shape into typed rows for every window', () => {
    // Given the committed endpoint payload
    // When it crosses the boundary
    const snapshot = parseAnalysisProvidersSnapshot(WIRE_SNAPSHOT)

    // Then every window is present with its counters intact
    expect(snapshot.generatedAt).toBe(1_776_041_600)
    expect(snapshot.windows.last24h).toEqual([
      { provider: 'tavily', requests: 1607, success: 1500, errors: 100, quotaExhausted: 7, credits: 1498 },
      { provider: 'serpapi', requests: 41, success: 39, errors: 2, quotaExhausted: 0, credits: 39 },
    ])
    expect(snapshot.windows.last7d[0]?.requests).toBe(9842)
    expect(snapshot.windows.last30d[0]?.provider).toBe('brave')
    expect(ANALYSIS_PROVIDER_WINDOW_KEYS.every((key) => Array.isArray(snapshot.windows[key]))).toBe(true)
  })

  it('coalesces an absent or null provider to tavily and keeps an unknown id verbatim', () => {
    const snapshot = parseAnalysisProvidersSnapshot({
      generated_at: 1,
      windows: {
        last24h: [
          { requests: 3, success: 3, errors: 0, quota_exhausted: 0, credits: 3 },
          { provider: null, requests: 2, success: 1, errors: 1, quota_exhausted: 0, credits: 1 },
          { provider: 'openai', requests: 1, success: 1, errors: 0, quota_exhausted: 0, credits: 1 },
        ],
      },
    })

    // Legacy NULL rows merge to tavily server-side and again at the boundary,
    // while a future provider is echoed instead of being mislabeled as Tavily.
    expect(snapshot.windows.last24h.map((row) => row.provider)).toEqual(['tavily', 'tavily', 'openai'])
  })

  it('reads a malformed row as zero counters instead of throwing', () => {
    const snapshot = parseAnalysisProvidersSnapshot({
      generated_at: 'yesterday',
      windows: {
        last7d: [
          { provider: 'exa', requests: 12, success: 'ten', credits: null },
          'not-a-row',
        ],
      },
    })

    // A boundary parse degrades to zeros; it never throws into the view.
    expect(snapshot.generatedAt).toBe(0)
    expect(snapshot.windows.last7d).toEqual([
      { provider: 'exa', requests: 12, success: 0, errors: 0, quotaExhausted: 0, credits: 0 },
      { provider: 'tavily', requests: 0, success: 0, errors: 0, quotaExhausted: 0, credits: 0 },
    ])
  })

  it('yields empty windows instead of throwing when the payload is missing them', () => {
    expect(parseAnalysisProvidersSnapshot(null).windows).toEqual({ last24h: [], last7d: [], last30d: [] })
    expect(parseAnalysisProvidersSnapshot({ generated_at: 7, windows: { last24h: 'nope' } }).windows.last24h)
      .toEqual([])
    expect(parseAnalysisProvidersSnapshot({ windows: { last30d: [{ provider: 'exa' }] } }).windows.last30d)
      .toEqual([{ provider: 'exa', requests: 0, success: 0, errors: 0, quotaExhausted: 0, credits: 0 }])
  })
})

describe('fetchAdminAnalysisProviders', () => {
  it('requests the single snapshot endpoint and returns the parsed windows', async () => {
    // Given the endpoint answers the frozen wire shape
    const fetchMock = mock((_input: RequestInfo | URL) =>
      Promise.resolve(
        new Response(JSON.stringify(WIRE_SNAPSHOT), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    )
    globalThis.fetch = fetchMock as unknown as typeof fetch

    // When the view loads
    const snapshot = await fetchAdminAnalysisProviders()

    // Then one request carries all three windows (no per-tab refetch)
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect((fetchMock.mock.calls[0] as [string])[0]).toBe('/api/analysis/providers')
    expect(snapshot.windows.last24h[0]?.requests).toBe(1607)
    expect(snapshot.windows.last30d[0]?.credits).toBe(1862)
  })
})
