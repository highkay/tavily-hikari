import '../../test/happydom'

import { afterEach, beforeEach, describe, expect, it } from 'bun:test'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import type { AnalysisProvidersSnapshot } from '../api'
import { analysisProvidersTranslations } from '../i18n/translations/analysisProviders'
import AnalysisProvidersScreen, { selectAnalysisProviderRows } from './AnalysisProvidersScreen'

const roots: Root[] = []
const originalFetch = window.fetch
let fetchCount = 0

const snapshot: AnalysisProvidersSnapshot = {
  generatedAt: 1_776_041_600,
  windows: {
    last24h: [
      { provider: 'tavily', requests: 1607, success: 1500, errors: 100, quotaExhausted: 7, credits: 1498 },
      { provider: 'serpapi', requests: 41, success: 39, errors: 2, quotaExhausted: 0, credits: 39 },
    ],
    last7d: [
      { provider: 'exa', requests: 517, success: 498, errors: 19, quotaExhausted: 0, credits: 495 },
    ],
    last30d: [
      { provider: 'brave', requests: 1980, success: 1871, errors: 96, quotaExhausted: 13, credits: 1862 },
      { provider: 'tavily', requests: 1200, success: 1100, errors: 90, quotaExhausted: 10, credits: 1090 },
    ],
  },
}

function render(props: Partial<Parameters<typeof AnalysisProvidersScreen>[0]> = {}): HTMLElement {
  const container = document.createElement('div')
  document.body.appendChild(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => {
    root.render(
      <AnalysisProvidersScreen
        snapshot={snapshot}
        loading={false}
        error={null}
        language="en"
        onRetry={() => undefined}
        {...props}
      />,
    )
  })
  return container
}

function rows(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>('tbody tr[data-provider]'))
}

function cells(row: HTMLElement): string[] {
  return Array.from(row.querySelectorAll('td')).map((cell) => (cell.textContent ?? '').trim())
}

function clickWindow(container: HTMLElement, key: string): void {
  const tab = container.querySelector<HTMLButtonElement>(`[data-window="${key}"]`)
  if (tab === null) throw new Error(`No window tab for ${key}`)
  act(() => {
    tab.click()
  })
}

beforeEach(() => {
  fetchCount = 0
  // Any network use by the screen would be a contract break: the three windows
  // ride one snapshot, so tab switching must stay client-side.
  window.fetch = (() => {
    fetchCount += 1
    return Promise.resolve(new Response('{}', { status: 200 }))
  }) as unknown as typeof fetch
})

afterEach(() => {
  for (const root of roots.splice(0)) {
    act(() => root.unmount())
  }
  window.fetch = originalFetch
  fetchCount = 0
  document.body.innerHTML = ''
})

describe('selectAnalysisProviderRows', () => {
  it('reads the requested window and stays empty without a snapshot', () => {
    expect(selectAnalysisProviderRows(snapshot, 'last7d').map((row) => row.provider)).toEqual(['exa'])
    expect(selectAnalysisProviderRows(null, 'last24h')).toEqual([])
  })
})

describe('AnalysisProvidersScreen (contract C1)', () => {
  it('renders one row per provider with the 24h counters and the credits column', () => {
    const container = render()

    const rendered = rows(container)
    expect(rendered.map((row) => row.dataset.provider)).toEqual(['tavily', 'serpapi'])
    expect(cells(rendered[0]!)).toEqual(['Tavily', '1,607', '1,500', '100', '7', '1,498'])
    expect(container.querySelector('thead')?.textContent).toContain('Quota Exhausted')
    expect(container.querySelector('thead')?.textContent).toContain('Credits')
  })

  it('switches windows client-side over the same snapshot without refetching', () => {
    const container = render()

    clickWindow(container, 'last30d')

    // The 30d tab shows its own rows from the one snapshot already in memory.
    expect(rows(container).map((row) => row.dataset.provider)).toEqual(['brave', 'tavily'])
    expect(cells(rows(container)[0]!)).toEqual(['Brave', '1,980', '1,871', '96', '13', '1,862'])
    expect(container.querySelector('[data-window="last30d"]')?.getAttribute('aria-checked')).toBe('true')
    expect(container.querySelector('[data-window="last24h"]')?.getAttribute('aria-checked')).toBe('false')
    expect(fetchCount).toBe(0)
  })

  it('renders the Chinese catalog labels', () => {
    const container = render({ language: 'zh' })

    const text = container.textContent ?? ''
    expect(text).toContain(analysisProvidersTranslations.zh.tabs.last24h)
    expect(text).toContain(analysisProvidersTranslations.zh.table.quotaExhausted)
    expect(text).toContain(analysisProvidersTranslations.zh.table.credits)
  })

  it('shows the window empty state when the snapshot has no rows for it', () => {
    const container = render({
      snapshot: { generatedAt: 1, windows: { last24h: [], last7d: [], last30d: [] } },
    })

    expect(rows(container)).toEqual([])
    expect(container.textContent).toContain(analysisProvidersTranslations.en.noRows)
  })

  it('keeps the loading region before the first snapshot resolves', () => {
    const container = render({ snapshot: null, loading: true })

    expect(container.querySelector('.admin-loading-region')).not.toBeNull()
    expect(container.textContent).toContain(analysisProvidersTranslations.en.loading)
  })

  it('surfaces a load failure with a retry affordance and calls onRetry', () => {
    let retried = 0
    const container = render({ snapshot: null, error: 'upstream unavailable', onRetry: () => { retried += 1 } })

    expect(container.querySelector('[role="alert"]')?.textContent).toContain('upstream unavailable')
    const retry = container.querySelector<HTMLButtonElement>('button.btn-outline')
    expect(retry?.textContent).toBe(analysisProvidersTranslations.en.retry)
    act(() => {
      retry?.click()
    })
    expect(retried).toBe(1)
  })

  it('keeps the last good snapshot and offers retry when a refresh fails', () => {
    const container = render({ error: 'refresh failed' })

    expect(rows(container).map((row) => row.dataset.provider)).toEqual(['tavily', 'serpapi'])
    expect(container.querySelector('.alert')?.textContent).toContain('refresh failed')
  })
})
