import type { Language } from '../types'

/**
 * `分析 > 提供方` copy for both catalogs (multi-provider-alignment-completion C1).
 *
 * Declared inside this fragment module — never in `i18n/types.ts`, which is at its
 * source line budget — following the `tokenDetail.ts` precedent. Consumers index
 * {@link analysisProvidersTranslations} with the active `language` prop
 * (`DashboardOverview.tsx` `adminDashboardProvidersTranslations.en` precedent), so
 * the shared runtime catalogs and the catalog contract stay untouched.
 */
export interface AnalysisProvidersTranslations {
  /** Sidebar child label under 分析 / Analytics. */
  nav: string
  title: string
  description: string
  /** `aria-label` of the window radiogroup. */
  tabsLabel: string
  tabs: { last24h: string; last7d: string; last30d: string }
  table: {
    provider: string
    requests: string
    success: string
    errors: string
    quotaExhausted: string
    credits: string
  }
  /** Why the credits column is an estimate (no upstream billing join in v1). */
  creditsHint: string
  generatedAt: string
  refreshing: string
  loading: string
  errorTitle: string
  retry: string
  emptyTitle: string
  emptyDescription: string
  noRows: string
}

export const analysisProvidersTranslations: Record<Language, AnalysisProvidersTranslations> = {
  en: {
    nav: 'Providers',
    title: 'Provider Analysis',
    description:
      'Per-provider request, success, error, quota-exhausted, and credit totals for the selected window.',
    tabsLabel: 'Analysis window',
    tabs: { last24h: 'Last 24 hours', last7d: 'Last 7 days', last30d: 'Last 30 days' },
    table: {
      provider: 'Provider',
      requests: 'Requests',
      success: 'Success',
      errors: 'Errors',
      quotaExhausted: 'Quota Exhausted',
      credits: 'Credits',
    },
    creditsHint: 'Local-estimated business credits',
    generatedAt: 'Generated at',
    refreshing: 'Refreshing…',
    loading: 'Loading provider analysis…',
    errorTitle: 'Provider analysis unavailable',
    retry: 'Retry',
    emptyTitle: 'No provider snapshot yet',
    emptyDescription: 'Provider analysis appears once request activity has been recorded.',
    noRows: 'No provider activity in this window.',
  },
  zh: {
    nav: '提供方',
    title: '提供方分析',
    description: '按提供方统计所选窗口的请求数、成功、错误、配额耗尽与积分。',
    tabsLabel: '统计窗口',
    tabs: { last24h: '最近24小时', last7d: '最近7天', last30d: '最近30天' },
    table: {
      provider: '提供方',
      requests: '请求数',
      success: '成功',
      errors: '错误',
      quotaExhausted: '配额耗尽',
      credits: '积分',
    },
    creditsHint: '本地估算业务积分',
    generatedAt: '生成于',
    refreshing: '正在刷新…',
    loading: '正在加载提供方分析…',
    errorTitle: '提供方分析不可用',
    retry: '重试',
    emptyTitle: '暂无提供方快照',
    emptyDescription: '产生请求活动后即可查看提供方分析。',
    noRows: '该窗口内暂无提供方活动。',
  },
}
