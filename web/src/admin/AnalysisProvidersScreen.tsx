import { useMemo, useState } from 'react'

import {
  ANALYSIS_PROVIDER_WINDOW_KEYS,
  type AnalysisProviderRow,
  type AnalysisProvidersSnapshot,
  type AnalysisProviderWindowKey,
} from '../api'
import type { Language } from '../i18n'
import { analysisProvidersTranslations } from '../i18n/translations/analysisProviders'
import { logProviderDisplayLabel } from '../lib/logProvider'
import AdminLoadingRegion from '../components/AdminLoadingRegion'

/**
 * 分析 > 提供方 (multi-provider-alignment-completion C1).
 *
 * One `GET /api/analysis/providers` snapshot carries all three windows, so the
 * 24h/7d/30d tab strip is pure client-side state over `snapshot.windows` and
 * never re-fetches. Structure mirrors `PressureAnalysisScreen`: the runtime owns
 * the fetch and passes snapshot/loading/error plus a retry, and the copy comes
 * from the `analysisProviders.ts` fragment resolved by the active `language`
 * (the shared catalog contract has no line headroom for a new `admin.*` block).
 */
export interface AnalysisProvidersScreenProps {
  snapshot: AnalysisProvidersSnapshot | null
  loading: boolean
  error: string | null
  language: Language
  onRetry: () => void
}

/** Rows of one window; an absent snapshot renders nothing rather than throwing. */
export function selectAnalysisProviderRows(
  snapshot: AnalysisProvidersSnapshot | null,
  windowKey: AnalysisProviderWindowKey,
): AnalysisProviderRow[] {
  return snapshot?.windows[windowKey] ?? []
}

function createFormatter(language: Language, maximumFractionDigits: number): Intl.NumberFormat {
  return new Intl.NumberFormat(language === 'zh' ? 'zh-CN' : 'en-US', {
    maximumFractionDigits,
  })
}

function formatGeneratedAt(language: Language, generatedAt: number): string {
  if (!Number.isFinite(generatedAt) || generatedAt <= 0) return '—'
  return new Intl.DateTimeFormat(language === 'zh' ? 'zh-CN' : 'en-US', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(new Date(generatedAt * 1000))
}

export default function AnalysisProvidersScreen({
  snapshot,
  loading,
  error,
  language,
  onRetry,
}: AnalysisProvidersScreenProps): JSX.Element {
  const strings = analysisProvidersTranslations[language]
  const [windowKey, setWindowKey] = useState<AnalysisProviderWindowKey>('last24h')
  const rows = useMemo(() => selectAnalysisProviderRows(snapshot, windowKey), [snapshot, windowKey])
  const countFormat = useMemo(() => createFormatter(language, 0), [language])
  const creditsFormat = useMemo(() => createFormatter(language, 2), [language])

  if (loading && !snapshot) {
    return (
      <AdminLoadingRegion
        loadState="initial_loading"
        loadingLabel={strings.loading}
        minHeight={420}
      />
    )
  }

  if (error && !snapshot) {
    return (
      <section className="surface panel analysis-providers-empty-state" role="alert">
        <h2>{strings.errorTitle}</h2>
        <p className="panel-description">{error}</p>
        <button type="button" className="btn btn-outline" onClick={onRetry}>
          {strings.retry}
        </button>
      </section>
    )
  }

  if (!snapshot) {
    return (
      <section className="surface panel analysis-providers-empty-state">
        <h2>{strings.emptyTitle}</h2>
        <p className="panel-description">{strings.emptyDescription}</p>
      </section>
    )
  }

  return (
    <div className="analysis-providers-page" data-testid="analysis-providers-screen">
      <section className="admin-rankings-toolbar-band" aria-label={strings.tabsLabel}>
        <div className="admin-rankings-tab-strip" role="radiogroup" aria-label={strings.tabsLabel}>
          {ANALYSIS_PROVIDER_WINDOW_KEYS.map((key) => {
            const active = key === windowKey
            return (
              <button
                key={key}
                type="button"
                role="radio"
                aria-checked={active}
                data-window={key}
                className={`admin-rankings-tab ${active ? 'is-active' : ''}`}
                onClick={() => setWindowKey(key)}
              >
                {strings.tabs[key]}
              </button>
            )
          })}
        </div>
      </section>

      {error ? (
        <div className="alert">
          <div>{error}</div>
          <div className="admin-ranking-inline-actions">
            <button type="button" className="btn btn-outline btn-sm" onClick={onRetry}>
              {strings.retry}
            </button>
          </div>
        </div>
      ) : null}

      <section className="surface panel analysis-providers-panel">
        <div className="panel-header">
          <div>
            <h2>{strings.tabs[windowKey]}</h2>
          </div>
          <div className="admin-ranking-stale-hint">
            {loading ? strings.refreshing : `${strings.generatedAt} ${formatGeneratedAt(language, snapshot.generatedAt)}`}
          </div>
        </div>
        {rows.length === 0 ? (
          <div className="empty-state alert">{strings.noRows}</div>
        ) : (
          <div className="table-wrapper jobs-table-wrapper">
            <table className="jobs-table admin-users-table" data-table-density="compact">
              <thead>
                <tr>
                  <th scope="col">{strings.table.provider}</th>
                  <th scope="col">{strings.table.requests}</th>
                  <th scope="col">{strings.table.success}</th>
                  <th scope="col">{strings.table.errors}</th>
                  <th scope="col">{strings.table.quotaExhausted}</th>
                  <th scope="col" title={strings.creditsHint}>{strings.table.credits}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <tr key={row.provider} data-provider={row.provider}>
                    <td><strong>{logProviderDisplayLabel(row.provider)}</strong></td>
                    <td>{countFormat.format(row.requests)}</td>
                    <td>{countFormat.format(row.success)}</td>
                    <td>{countFormat.format(row.errors)}</td>
                    <td>{countFormat.format(row.quotaExhausted)}</td>
                    <td>{creditsFormat.format(row.credits)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  )
}
