import '../../test/happydom'

import { afterEach, describe, expect, it } from 'bun:test'
import { act } from 'react'
import { createElement } from 'react'
import { createRoot } from 'react-dom/client'
import { renderToStaticMarkup } from 'react-dom/server'

import type { SummaryProviderCount } from '../api'
import meta, * as dashboardStories from './DashboardOverview.stories'
import { buildRollingHourlyWindow } from './dashboardHourlyCharts'

afterEach(() => {
  document.body.innerHTML = ''
})

describe('DashboardOverview Storybook coverage', () => {
  it('keeps all six chart modes and the empty-selection story available', () => {
    expect(meta).toMatchObject({ title: 'Admin/Components/DashboardOverview' })
    expect(dashboardStories.Default).toMatchObject({})
    expect(dashboardStories.TypesMode).toMatchObject({})
    expect(dashboardStories.CreditsMode).toMatchObject({})
    expect(dashboardStories.ResultsAreaMode).toMatchObject({})
    expect(dashboardStories.TypesAreaMode).toMatchObject({})
    expect(dashboardStories.CreditsAreaMode).toMatchObject({})
    expect(dashboardStories.TypesAreaHiddenMiddleSeries).toMatchObject({})
    expect(dashboardStories.CreditsAreaLocalOnly).toMatchObject({})
    expect(dashboardStories.CreditsAreaNoUpstreamSamples).toMatchObject({})
    expect(dashboardStories.IntegrityRepairing).toMatchObject({})
    expect(dashboardStories.CreditsEmptyData).toMatchObject({})
    expect(dashboardStories.HiddenSeriesEmpty).toMatchObject({})
    expect(dashboardStories.RecentAlertsDesktopEvidence).toMatchObject({})
    expect(dashboardStories.RecentAlertsBusinessHourWindow).toMatchObject({})
    expect(dashboardStories.NoPreviousMonthComparison).toMatchObject({})
    expect(dashboardStories.ProviderActivity).toMatchObject({})
  })

  it('renders a repair state and leaves the affected chart interval unverified', () => {
    const args = dashboardStories.IntegrityRepairing.args
    const markup = renderToStaticMarkup(createElement(meta.component, args as never))
    expect(markup).toContain('Repairing statistics')
  })

  it('renders the empty-selection story with the updated server-time copy', () => {
    const args = dashboardStories.HiddenSeriesEmpty.args
    expect(args).toBeDefined()
    const markup = renderToStaticMarkup(createElement(meta.component, args as never))
    const expectedCurrentBuckets = buildRollingHourlyWindow(args?.hourlyRequestWindow).slots.length
    expect(markup).not.toContain('dashboard-hero-panel')
    expect(markup).not.toContain('Operations Dashboard')
    expect(markup).not.toContain('Global health, risk signals, and actionable activity in one place.')
    expect(markup).toContain('No visible chart series for the current selection.')
    expect(markup).toContain('Traffic Trends')
    expect(markup).toContain(
      `Local time axis · 24 full hours + current hour (${expectedCurrentBuckets} slots`,
    )
    expect(markup).toContain('Compare requests and credits')
    expect(markup).toContain('dashboard-summary-card-backdrop')
  })

  it('renders the grouped alert queue summary in the default story', () => {
    const args = dashboardStories.Default.args
    const markup = renderToStaticMarkup(createElement(meta.component, args as never))
    expect(markup).toContain('1 hour')
    expect(markup).toContain('24 hours')
    expect(markup).toContain('7 days')
    expect(markup).toContain('24-hour queue')
    expect(markup).toContain('Alert window')
    expect(markup).toContain('Review')
    expect(markup).toContain('Alice Wang')
    expect(markup).toContain('Tavily Search')
    expect(markup).toContain('User request rate limited · 5m window')
    expect(markup).toContain('Upstream key blocked')
    expect(markup).not.toContain('User request rate limited · Tavily Search')
    expect(markup).not.toContain('Upstream key blocked · Upstream account deactivated')
    expect(markup).not.toContain('Local request-rate limit')
    expect(markup).toContain('rolling 5m request-rate window')
    expect(markup).toContain('Queue below')
    expect(markup).toContain('Review group')
    expect(markup).toContain('aria-label="Review group: Alice Wang"')
    expect(markup).toContain('dashboard-alerts-summary__subject-button')
    expect(markup).toContain('aria-label="Open user: Alice Wang"')
    expect(markup).toContain('dashboard-alerts-summary__count-badge')
    expect(markup).toContain('status-pill-warning')
    expect(markup).toContain('>5<')
    expect(markup).toContain('dashboard-alerts-summary__field-label')
    expect(markup).not.toContain('deep-link into the grouped Alerts view')
  })

  it('prefers the real 60m business-call window over stale 5m group metadata', () => {
    const args = dashboardStories.RecentAlertsBusinessHourWindow.args
    const markup = renderToStaticMarkup(createElement(meta.component, args as never))
    expect(markup).toContain('User request rate limited · 60m window')
    expect(markup).not.toContain('User request rate limited · 5m window')
    expect(markup).toContain('rolling 60m request-rate window')
    expect(markup).not.toContain('rolling 5m request-rate window for MCP Search')
  })

  it('opens the user detail target from a grouped alert subject', async () => {
    const openedUsers: string[] = []
    const container = document.createElement('div')
    document.body.appendChild(container)
    const root = createRoot(container)
    const args = {
      ...dashboardStories.Default.args,
      initialVisibleResultSeries: [],
      onOpenUser: (userId: string) => openedUsers.push(userId),
    }

    await act(async () => {
      root.render(createElement(meta.component, args as never))
    })

    const subjectButton = container.querySelector<HTMLButtonElement>('.dashboard-alerts-summary__subject-button')
    expect(subjectButton?.textContent).toBe('Alice Wang')

    await act(async () => {
      subjectButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })

    expect(openedUsers).toEqual(['usr_001'])

    await act(async () => {
      root.unmount()
    })
  })

  it('exposes credit stories for normal, local-only, and unsampled states', () => {
    expect(dashboardStories.CreditsMode.args?.initialChartMode).toBe('credits')
    expect(dashboardStories.CreditsAreaMode.args?.initialChartMode).toBe('creditsArea')
    expect(dashboardStories.CreditsAreaLocalOnly.args?.initialVisibleCreditSeries).toEqual(['localEstimate'])
    expect(
      dashboardStories.CreditsAreaNoUpstreamSamples.args?.hourlyRequestWindow.buckets
        .every((bucket) => bucket.upstreamActualCredits == null),
    ).toBe(true)
    expect(dashboardStories.CreditsEmptyData.args?.hourlyRequestWindow.buckets).toEqual([])
  })

  it('keeps the month summary on a full natural-month axis with a visible previous-month comparison line', () => {
    const args = dashboardStories.Default.args
    expect(args?.monthSeries?.current).toHaveLength(31)
    expect(args?.monthSeries?.comparison).toHaveLength(31)
    expect(args?.monthSeries?.current.slice(7).every((point) => point.total == null)).toBe(true)
    expect(args?.monthSeries?.comparison.some((point) => typeof point.total === 'number')).toBe(true)
  })

  it('exposes an explicit empty-state story when previous-month comparison data is absent', () => {
    const args = dashboardStories.NoPreviousMonthComparison.args
    expect(args?.monthSeries?.comparison).toEqual([])
    const markup = renderToStaticMarkup(createElement(meta.component, args as never))
    expect(markup).toContain('No retained previous-month comparison data.')
  })

  it('keeps the absolute charts on all-series defaults in the primary stories', () => {
    expect(dashboardStories.Default.args?.initialVisibleResultSeries).toBeUndefined()
    expect(dashboardStories.Default.args?.initialVisibleTypeSeries).toBeUndefined()
    expect(dashboardStories.CreditsMode.args?.initialVisibleCreditSeries).toBeUndefined()
  })

  it('exposes dedicated area-chart stories for all three series families', () => {
    expect(dashboardStories.ResultsAreaMode.args?.initialChartMode).toBe('resultsArea')
    expect(dashboardStories.TypesAreaMode.args?.initialChartMode).toBe('typesArea')
    expect(dashboardStories.CreditsAreaMode.args?.initialChartMode).toBe('creditsArea')
  })

  it('exposes a stacked-area hidden-middle-series regression story', () => {
    expect(dashboardStories.TypesAreaHiddenMiddleSeries.args?.initialChartMode).toBe('typesArea')
    expect(dashboardStories.TypesAreaHiddenMiddleSeries.args?.initialVisibleTypeSeries).toEqual([
      'mcpNonBillable',
      'apiNonBillable',
      'apiBillable',
    ])
  })

  it('renders the provider activity card with exact per-provider counts from the fixture', async () => {
    const container = document.createElement('div')
    document.body.appendChild(container)
    const root = createRoot(container)
    await act(async () => {
      root.render(
        createElement(meta.component, {
          ...dashboardStories.ProviderActivity.args,
          initialVisibleResultSeries: [],
          initialVisibleTypeSeries: [],
          initialVisibleCreditSeries: [],
        } as never),
      )
    })

    const card = container.querySelector('.dashboard-provider-panel')
    expect(card).not.toBeNull()
    expect(card?.querySelector('h2')?.textContent).toBe('Provider activity')
    const expected: Record<string, string[]> = {
      tavily: ['Tavily', '1,607', '1,521', '61', '25'],
      serpapi: ['SerpApi', '413', '398', '12', '3'],
      exa: ['Exa', '128', '121', '7', '0'],
      brave: ['Brave', '96', '88', '6', '2'],
    }
    for (const [provider, cells] of Object.entries(expected)) {
      const row = card?.querySelector(`tr[data-provider="${provider}"]`)
      expect(row).not.toBeNull()
      const text = Array.from(row?.querySelectorAll('td') ?? []).map((cell) => cell.textContent)
      expect(text).toEqual(cells)
    }

    await act(async () => {
      root.unmount()
    })
  })

  it('re-renders provider counts when the fixture changes', async () => {
    const container = document.createElement('div')
    document.body.appendChild(container)
    const root = createRoot(container)
    const baseArgs = {
      ...dashboardStories.ProviderActivity.args,
      initialVisibleResultSeries: [],
      initialVisibleTypeSeries: [],
      initialVisibleCreditSeries: [],
    }
    const readCells = () =>
      Array.from(container.querySelectorAll('.dashboard-provider-panel tbody td')).map((cell) => cell.textContent)
    const first: SummaryProviderCount[] = [
      { provider: 'tavily', total_requests: 1607, success_count: 1521, error_count: 61, quota_exhausted_count: 25 },
      { provider: 'serpapi', total_requests: 413, success_count: 398, error_count: 12, quota_exhausted_count: 3 },
    ]
    await act(async () => {
      root.render(createElement(meta.component, { ...baseArgs, providerCounts: first } as never))
    })
    expect(readCells()).toEqual(['Tavily', '1,607', '1,521', '61', '25', 'SerpApi', '413', '398', '12', '3'])

    const second: SummaryProviderCount[] = [
      { provider: 'tavily', total_requests: 1607, success_count: 1521, error_count: 61, quota_exhausted_count: 25 },
      { provider: 'serpapi', total_requests: 509, success_count: 490, error_count: 16, quota_exhausted_count: 3 },
    ]
    await act(async () => {
      root.render(createElement(meta.component, { ...baseArgs, providerCounts: second } as never))
    })
    expect(readCells()).toEqual(['Tavily', '1,607', '1,521', '61', '25', 'SerpApi', '509', '490', '16', '3'])

    await act(async () => {
      root.unmount()
    })
  })

  it('hides the provider activity card when provider counts are absent or empty', () => {
    const withoutCounts = renderToStaticMarkup(createElement(meta.component, dashboardStories.Default.args as never))
    expect(withoutCounts).not.toContain('dashboard-provider-panel')
    expect(withoutCounts).not.toContain('Provider activity')

    const withEmpty = renderToStaticMarkup(
      createElement(meta.component, { ...dashboardStories.Default.args, providerCounts: [] } as never),
    )
    expect(withEmpty).not.toContain('dashboard-provider-panel')
    expect(withEmpty).not.toContain('Provider activity')
  })
})
