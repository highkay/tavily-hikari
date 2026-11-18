import type { Meta, StoryObj } from '@storybook/react-vite'

import type { ApiKeyStats } from '../api'
import { useTranslate } from '../i18n'
import { KeyQuotaPanel, KeyUsageSyncButton } from './keyProviderQuota'

const numberFormatter = new Intl.NumberFormat('en-US')
const timestampFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: 'medium',
  timeStyle: 'medium',
})

function providerDetail(provider: 'keenable' | 'zhihu'): ApiKeyStats {
  const isZhihu = provider === 'zhihu'
  return {
    id: `${provider}-story-key`,
    provider,
    status: 'active',
    group: 'production',
    registration_ip: null,
    registration_region: null,
    status_changed_at: null,
    last_used_at: null,
    deleted_at: null,
    quota_limit: isZhihu ? 10 : null,
    quota_remaining: isZhihu ? 3 : null,
    quota_renews_at: isZhihu ? 1_800_000_000 : null,
    quota_synced_at: isZhihu ? 1_700_000_000 : null,
    total_requests: 0,
    success_count: 0,
    error_count: 0,
    quota_exhausted_count: 0,
    quarantine: null,
    transient_backoff: null,
  }
}

function ProviderQuotaProof(): JSX.Element {
  const shared = useTranslate().admin.providers.shared
  return (
    <div className="grid gap-6 p-4 sm:p-6 xl:grid-cols-2">
      {(['keenable', 'zhihu'] as const).map((provider) => (
        <div key={provider} className="grid content-start gap-3">
          <KeyQuotaPanel
            detail={providerDetail(provider)}
            loadState="ready"
            loadingLabel="Loading"
            syncNotice={provider === 'keenable' ? shared.syncPassiveNotice : null}
            formatNumber={(value) => numberFormatter.format(value)}
            formatTimestamp={(value) => (value == null ? '—' : timestampFormatter.format(new Date(value * 1000)))}
            formatPercent={(numerator, denominator) =>
              denominator > 0 ? `${Math.round((numerator / denominator) * 100)}%` : '—'
            }
          />
          <div className="flex justify-end">
            <KeyUsageSyncButton provider={provider} syncState="idle" onSync={() => undefined} />
          </div>
        </div>
      ))}
    </div>
  )
}

const meta = {
  title: 'Admin/Fragments/KeyProviderQuota',
  component: ProviderQuotaProof,
  tags: ['autodocs'],
  parameters: { layout: 'fullscreen' },
} satisfies Meta<typeof ProviderQuotaProof>

export default meta

type Story = StoryObj<typeof meta>

export const ProviderQuotas: Story = {
  globals: { language: 'en', themeMode: 'light' },
  parameters: { viewport: { defaultViewport: '1440-device-desktop' } },
  play: ({ canvasElement }) => {
    const missingValues = Array.from(
      canvasElement.querySelectorAll('[data-provider="keenable"] .metric-value'),
      (node) => node.textContent,
    )
    if (missingValues.join(',') !== '--,--') {
      throw new Error('Expected a missing Keenable snapshot to render --, never 0')
    }
    const zhihu = canvasElement.querySelector('[data-provider="zhihu"]')?.textContent ?? ''
    for (const expected of ['Daily remaining', '3', 'Renews']) {
      if (!zhihu.includes(expected)) {
        throw new Error(`Expected Zhihu daily quota content: ${expected}`)
      }
    }
  },
}

export const ProviderQuotasDark: Story = {
  ...ProviderQuotas,
  globals: { language: 'en', themeMode: 'dark' },
}

export const ProviderQuotasMobile: Story = {
  ...ProviderQuotas,
  parameters: { viewport: { defaultViewport: 'mobile1' } },
}

export const ProviderQuotasDarkMobile: Story = {
  ...ProviderQuotas,
  globals: { language: 'zh', themeMode: 'dark' },
  parameters: { viewport: { defaultViewport: 'mobile1' } },
  play: ({ canvasElement }) => {
    const zhihu = canvasElement.querySelector('[data-provider="zhihu"]')?.textContent ?? ''
    if (!zhihu.includes('每日剩余') || !zhihu.includes('续费')) {
      throw new Error('Expected localized Zhihu daily quota content')
    }
  },
}
