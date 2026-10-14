import { describe, expect, it } from 'bun:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'

import type { PublicTokenLog } from './api'
import PublicHomeHeroCard from './components/PublicHomeHeroCard'
import { translations } from './i18n'
import { ThemeProvider } from './theme'
import { PublicLogsPanels, __testables } from './PublicHome'
import { mixedProviderPublicLogs } from './PublicHome.stories'

describe('PublicHome guide token visibility', () => {
  const placeholder = 'th-xxxx-xxxxxxxxxxxx'
  const storedToken = 'th-a1b2-1234567890abcdef'

  it('keeps the legacy full-token hash bootstrap for the public homepage', () => {
    expect(__testables.resolveInitialTokenFromHash(`#${storedToken}`, {})).toBe(storedToken)
  })

  it('keeps the legacy token-id hash bootstrap for the public homepage', () => {
    expect(__testables.resolveInitialTokenFromHash('#a1b2', { a1b2: storedToken })).toBe(storedToken)
  })

  it('returns null when the homepage hash does not map to a known token', () => {
    expect(__testables.resolveInitialTokenFromHash('#unknown', { a1b2: storedToken })).toBeNull()
    expect(__testables.resolveInitialTokenFromHash('', { a1b2: storedToken })).toBeNull()
  })

  it('keeps the guide masked by default even when a full token is available', () => {
    expect(__testables.resolvePublicGuideToken('th-a1b2-1234567890abcdef', placeholder, false)).toBe(placeholder)
  })

  it('reveals the full token only when explicitly toggled on with a valid secret', () => {
    expect(__testables.resolvePublicGuideToken('th-a1b2-1234567890abcdef', placeholder, true)).toBe(
      'th-a1b2-1234567890abcdef',
    )
  })

  it('hides the guide immediately when the revealed token no longer matches the current token', () => {
    expect(__testables.shouldRevealPublicGuideToken(
      'th-b2c3-fedcba0987654321',
      'th-a1b2-1234567890abcdef',
    )).toBe(false)
  })

  it('falls back to the placeholder when the current value is incomplete', () => {
    expect(__testables.resolvePublicGuideToken('th-a1b2-', placeholder, true)).toBe(placeholder)
    expect(__testables.resolvePublicGuideToken('', placeholder, true)).toBe(placeholder)
  })

  it('normalizes legacy single-sample guides into an array and preserves multi-sample guides', () => {
    expect(__testables.resolveGuideSamples({
      title: 'Legacy',
      steps: [],
      sampleTitle: 'Example',
      snippetLanguage: 'bash',
      snippet: 'echo ok',
      reference: { label: 'Docs', url: 'https://example.com' },
    })).toEqual([
      {
        title: 'Example',
        language: 'bash',
        snippet: 'echo ok',
        reference: { label: 'Docs', url: 'https://example.com' },
      },
    ])

    const samples = [
      { title: 'One', language: 'json', snippet: '{}' },
      { title: 'Two', language: 'bash', snippet: 'curl ...' },
    ]
    expect(__testables.resolveGuideSamples({ title: 'Modern', steps: [], samples })).toBe(samples)
  })
})

describe('PublicHome provider column (plan todo 7 / M2)', () => {
  const renderLogsPanels = (logs: PublicTokenLog[], language: 'en' | 'zh' = 'en') =>
    renderToStaticMarkup(
      createElement(PublicLogsPanels, {
        logs,
        strings: translations[language].public.logs,
        expandedLogs: new Set<number>([mixedProviderPublicLogs[1].id]),
        onToggleLog: () => {},
      }),
    )

  it('renders the Provider column between Upstream and Result with per-row provider labels', () => {
    const markup = renderLogsPanels(mixedProviderPublicLogs)

    const upstreamHeader = markup.indexOf('>Upstream<')
    const providerHeader = markup.indexOf('>Provider<')
    const resultHeader = markup.indexOf('>Result<')
    expect(upstreamHeader).toBeGreaterThanOrEqual(0)
    expect(providerHeader).toBeGreaterThan(upstreamHeader)
    expect(resultHeader).toBeGreaterThan(providerHeader)
    expect(markup).not.toContain('<th>Tavily</th>')

    // Desktop table cells.
    expect(markup).toContain('<td>Tavily</td>')
    expect(markup).toContain('<td>SerpApi</td>')
    // Compact mobile cards carry the same provider label row.
    expect(markup).toContain('<strong>Tavily</strong>')
    expect(markup).toContain('<strong>SerpApi</strong>')
    // Exactly one column added: five headers in total.
    expect(markup.match(/<th>/g)).toHaveLength(5)
    // The expanded detail row spans the widened table.
    expect(markup).toContain('colSpan="5"')
  })

  it('localizes the provider column header in zh', () => {
    const markup = renderLogsPanels(mixedProviderPublicLogs, 'zh')
    expect(markup).toContain('>提供方<')
    expect(markup).toContain('>上游<')
    expect(markup).toContain('<td>SerpApi</td>')
  })

  it('merges a legacy null-provider row to the Tavily label', () => {
    const legacyRow: PublicTokenLog = { ...mixedProviderPublicLogs[0], id: 599, provider: null }
    const markup = renderLogsPanels([legacyRow])
    expect(markup).toContain('<td>Tavily</td>')
    expect(markup).toContain('<strong>Tavily</strong>')
    expect(markup).not.toContain('SerpApi')
  })

  it('renders the provider-neutral pool subtitle with no Tavily wording in zh', () => {
    const markup = renderToStaticMarkup(
      createElement(
        ThemeProvider,
        null,
        createElement(PublicHomeHeroCard, {
          publicStrings: translations.zh.public,
          metricsLoading: false,
          summaryLoading: false,
          metrics: { monthlySuccess: 1240, dailySuccess: 87 },
          availableKeys: 7,
          totalKeys: 12,
          error: null,
          showLinuxDoLogin: false,
          showTokenAccessButton: false,
          showAdminAction: false,
          adminActionLabel: 'Admin',
        }),
      ),
    )

    const poolSubtitle = translations.zh.public.metrics.pool.subtitle
    expect(poolSubtitle).not.toContain('Tavily')
    const subtitleMatch = markup.match(/<p class="metric-subtitle">([^<]*)<\/p>/)
    expect(subtitleMatch?.[1]).toBe(poolSubtitle)
  })
})
