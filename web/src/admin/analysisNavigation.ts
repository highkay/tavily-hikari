import type { AdminTranslations, Language } from '../i18n'
import {
  analysisProvidersTranslations,
  type AnalysisProvidersTranslations,
} from '../i18n/translations/analysisProviders'
import type { AdminNavSubItem, AdminNavTarget } from './AdminShell'
import type { AdminAnalysisView } from './routes'

/**
 * Analysis module navigation + header wiring, extracted from
 * `AdminDashboardRuntime.tsx` so the legacy runtime stays inside its source line
 * budget while the module gains sub-views (multi-provider-alignment-completion
 * C1 added `providers`).
 *
 * Both maps are `Record<AdminAnalysisView, …>`: adding a sub-view to
 * `AdminAnalysisView` without a nav target or an intro is a compile error here,
 * which is exactly the drift the previous nested ternaries hid.
 *
 * The providers copy lives in the `analysisProviders.ts` fragment (the shared
 * catalog contract `i18n/types.ts` has no line headroom), so it is resolved from
 * the active `language` rather than from `AdminTranslations`.
 */

/** Header copy the admin shell renders above an analysis sub-view. */
export interface AdminModuleIntro {
  title: string
  description: string
}

export function analysisProvidersStrings(language: Language): AnalysisProvidersTranslations {
  return analysisProvidersTranslations[language]
}

const ANALYSIS_NAV_TARGETS: Record<AdminAnalysisView, AdminNavTarget> = {
  rankings: 'analysis-rankings',
  usage: 'analysis-usage',
  pressure: 'analysis-pressure',
  providers: 'analysis-providers',
}

/** Sidebar children of the 分析 / Analytics entry, in display order. */
export function analysisNavChildren(
  strings: AdminTranslations,
  language: Language,
): AdminNavSubItem[] {
  return [
    { target: 'analysis-rankings', label: strings.nav.rankings },
    { target: 'analysis-usage', label: strings.nav.usage },
    { target: 'analysis-pressure', label: strings.nav.pressure },
    { target: 'analysis-providers', label: analysisProvidersTranslations[language].nav },
  ]
}

/** Sidebar target highlighted by an analysis route; defaults to rankings. */
export function activeAnalysisNavItem(view?: AdminAnalysisView | null): AdminNavTarget {
  return ANALYSIS_NAV_TARGETS[view ?? 'rankings']
}

/** Header title/description pair for the active analysis sub-view. */
export function analysisModuleIntro(
  view: AdminAnalysisView,
  strings: AdminTranslations,
  language: Language,
): AdminModuleIntro {
  switch (view) {
    case 'usage':
      return { title: strings.users.usage.title, description: strings.users.usage.description }
    case 'pressure':
      return { title: strings.pressure.title, description: strings.pressure.description }
    case 'providers': {
      const providers = analysisProvidersTranslations[language]
      return { title: providers.title, description: providers.description }
    }
    case 'rankings':
      return { title: strings.rankings.title, description: strings.rankings.description }
  }
}
