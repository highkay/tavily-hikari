import '../../test/happydom'

import { afterEach, describe, expect, it } from 'bun:test'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import type { UserProviderCount } from '../api'
import { normalizeUserDashboardOverview } from '../api/userConsoleNormalization'
import { adminDashboardProvidersTranslations } from '../i18n/translations/tokenDetail'
import UserDashboardOverview from './UserDashboardOverview'

const roots: Root[] = []

const text = {
  usage: 'Usage',
  description: 'Your API usage at a glance',
  dailySuccess: 'Daily success',
  dailyFailure: 'Daily failure',
  monthlySuccessUtc: 'Monthly success (UTC)',
  hourly: 'Business calls (1h)',
  daily: 'Daily credits',
  monthly: 'Monthly credits',
}

function overviewWith(providers: UserProviderCount[]) {
  return normalizeUserDashboardOverview({
    summary: { dailySuccess: 33, dailyFailure: 1, monthlySuccess: 512 },
    providers,
  })
}

function render(providers: UserProviderCount[], language: 'en' | 'zh' = 'en'): HTMLElement {
  const container = document.createElement('div')
  document.body.appendChild(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => {
    root.render(
      <UserDashboardOverview
        text={text}
        overview={overviewWith(providers)}
        loading={false}
        language={language}
        requestRateLabel="Request rate"
        formatNumber={(value) => new Intl.NumberFormat('en-US').format(value)}
      />,
    )
  })
  return container
}

function providerRows(container: HTMLElement): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>('.user-console-provider-table tbody tr[data-provider]'),
  )
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    act(() => root.unmount())
  }
  document.body.innerHTML = ''
})

describe('UserDashboardOverview provider card (contract C3)', () => {
  it('renders one row per provider from the overview breakdown', () => {
    const container = render([
      { provider: 'tavily', requests: 1284, success: 1201, errors: 83 },
      { provider: 'serpapi', requests: 312, success: 301, errors: 11 },
    ])

    const rows = providerRows(container)
    expect(rows.map((row) => row.dataset.provider)).toEqual(['tavily', 'serpapi'])
    expect(rows[0]?.textContent).toContain('Tavily')
    expect(rows[0]?.textContent).toContain('1,284')
    expect(rows[0]?.textContent).toContain('83')
    expect(container.querySelector('.user-console-provider-header')?.textContent).toBe(
      adminDashboardProvidersTranslations.en.title,
    )
  })

  it('hides the card when the overview carries no provider rows', () => {
    const container = render([])

    expect(container.querySelector('.user-console-provider-panel')).toBeNull()
    // The rest of the overview still renders.
    expect(container.querySelector('.user-console-summary-grid')).not.toBeNull()
  })

  it('uses the Chinese card labels in the zh catalog', () => {
    const container = render([{ provider: 'exa', requests: 9, success: 8, errors: 1 }], 'zh')

    expect(container.querySelector('.user-console-provider-header')?.textContent).toBe(
      adminDashboardProvidersTranslations.zh.title,
    )
    expect(container.textContent).toContain(adminDashboardProvidersTranslations.zh.requests)
  })
})
