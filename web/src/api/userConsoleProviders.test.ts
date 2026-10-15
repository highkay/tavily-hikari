import { describe, expect, it } from 'bun:test'

import { normalizeUserDashboardOverview } from './userConsoleNormalization'

describe('normalizeUserDashboardOverview providers (contract C3)', () => {
  it('parses the per-user provider rows of the overview payload', () => {
    // Given the extended user overview payload
    const overview = normalizeUserDashboardOverview({
      providers: [
        { provider: 'tavily', requests: 1284, success: 1201, errors: 83 },
        { provider: 'serpapi', requests: 312, success: 301, errors: 11 },
      ],
    })

    // Then the typed rows keep the user-scoped counters
    expect(overview.providers).toEqual([
      { provider: 'tavily', requests: 1284, success: 1201, errors: 83 },
      { provider: 'serpapi', requests: 312, success: 301, errors: 11 },
    ])
  })

  it('coalesces a blank provider to tavily and keeps an unknown id verbatim', () => {
    const overview = normalizeUserDashboardOverview({
      providers: [
        { requests: 5, success: 4, errors: 1 },
        { provider: 'openai', requests: 2, success: 2, errors: 0 },
      ],
    })

    // Legacy NULL rows merge to tavily; an unknown id is never relabeled.
    expect(overview.providers).toEqual([
      { provider: 'tavily', requests: 5, success: 4, errors: 1 },
      { provider: 'openai', requests: 2, success: 2, errors: 0 },
    ])
  })

  it('yields an empty list when the backend omits the breakdown so the card hides', () => {
    expect(normalizeUserDashboardOverview({}).providers).toEqual([])
    expect(normalizeUserDashboardOverview({ providers: null }).providers).toEqual([])
    expect(normalizeUserDashboardOverview(null).providers).toEqual([])
  })
})
