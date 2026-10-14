import type { Language } from '../types'

/**
 * `admin.tokenDetail.*` copy for both catalogs (multi-provider-ui-alignment
 * plan todo 3, matrix I1). Kept in a dedicated module so the shared runtime
 * catalogs stay within their source line budgets; `types.ts` references
 * {@link AdminTokenDetailTranslations} and `en.ts`/`zh.ts` spread the
 * matching record in one line each (same pattern as
 * `adminProviderTranslations`).
 *
 * Every string mirrors a previously hardcoded literal in
 * `pages/TokenDetail.tsx` so the page can be localized without copy drift.
 */
export interface AdminTokenDetailTranslations {
  sse: { title: string; live: string; offline: string }
  back: string
  regenerateSecret: string
  title: string
  tokenLabel: string
  info: {
    ariaLabel: string
    tokenId: string
    status: string
    enabled: string
    disabled: string
    totalRequests: string
    created: string
    lastUsed: string
    note: string
    unavailable: string
  }
  quickStats: {
    title: string
    description: string
    hourly: { label: string; description: string }
    daily: { label: string; description: string }
    monthly: { label: string; description: string }
    unavailable: string
  }
  snapshot: {
    title: string
    description: string
    period: string
    periodFilterAria: string
    start: string
    day: string
    week: string
    month: string
    requests: string
    success: string
    errors: string
    quotaExhausted: string
    logsEmpty: string
  }
  rotate: {
    title: string
    description: string
    cancel: string
    confirm: string
    inProgress: string
  }
  rotated: {
    title: string
    copiedDescription: string
    copyBlockedDescription: string
    close: string
    copy: string
    copied: string
    copyFailed: string
  }
  chart: {
    success: string
    systemLimited: string
    otherFailures: string
    requestsAxis: string
    loading: string
  }
}

/**
 * `admin.dashboard.providers.*` labels for the dashboard provider-activity
 * card (plan todo 3, matrix M3-labels). Per-provider row labels reuse the
 * existing `admin.providers.*` display names; these are the card title and
 * count columns only.
 */
export interface AdminDashboardProvidersTranslations {
  title: string
  requests: string
  success: string
  errors: string
  exhausted: string
}

export const adminTokenDetailTranslations: Record<Language, AdminTokenDetailTranslations> = {
  en: {
    sse: { title: 'Live updates via SSE', live: 'Live', offline: 'Offline' },
    back: 'Back',
    regenerateSecret: 'Regenerate Secret',
    title: 'Access Token Detail',
    tokenLabel: 'Token',
    info: {
      ariaLabel: 'Token metadata',
      tokenId: 'Token ID',
      status: 'Status',
      enabled: 'Enabled',
      disabled: 'Disabled',
      totalRequests: 'Total Requests',
      created: 'Created',
      lastUsed: 'Last Used',
      note: 'Note',
      unavailable: 'Token details are unavailable right now.',
    },
    quickStats: {
      title: 'Quick Stats',
      description: 'Rolling usage windows (1 hour / 24 hours / calendar month).',
      hourly: { label: '1 Hour', description: 'Rolling 1-hour window' },
      daily: { label: '24 Hours', description: 'Rolling 24-hour window' },
      monthly: { label: 'This Month', description: 'Calendar month' },
      unavailable: 'Token quota details are unavailable right now.',
    },
    snapshot: {
      title: 'Usage Snapshot',
      description: 'Aggregated metrics for the selected window.',
      period: 'Period',
      periodFilterAria: 'Period filter',
      start: 'Start',
      day: 'Day',
      week: 'Week',
      month: 'Month',
      requests: 'Requests',
      success: 'Success',
      errors: 'Errors',
      quotaExhausted: 'Quota Exhausted',
      logsEmpty: 'No logs yet.',
    },
    rotate: {
      title: 'Regenerate Token Secret',
      description:
        'This will invalidate the current token secret and generate a new one. The 4-char token ID will remain the same. Clients must be updated to use the new token.',
      cancel: 'Cancel',
      confirm: 'Regenerate',
      inProgress: 'Regenerating…',
    },
    rotated: {
      title: 'New Token Generated',
      copiedDescription: 'Full token copied to clipboard:',
      copyBlockedDescription: 'Automatic copy was blocked. The full token is selected below for manual copy.',
      close: 'Close',
      copy: 'Copy',
      copied: 'Copied',
      copyFailed: 'Copy Failed',
    },
    chart: {
      success: 'Success',
      systemLimited: 'System limited',
      otherFailures: 'Other failures',
      requestsAxis: 'Requests',
      loading: 'Loading…',
    },
  },
  zh: {
    sse: { title: '通过 SSE 实时更新', live: '实时', offline: '离线' },
    back: '返回',
    regenerateSecret: '重置 Secret',
    title: '访问令牌详情',
    tokenLabel: 'Token',
    info: {
      ariaLabel: 'Token 元数据',
      tokenId: 'Token ID',
      status: '状态',
      enabled: '已启用',
      disabled: '已禁用',
      totalRequests: '总请求数',
      created: '创建时间',
      lastUsed: '上次使用',
      note: '备注',
      unavailable: '暂时无法获取 Token 详情。',
    },
    quickStats: {
      title: '快速统计',
      description: '滚动用量窗口（1 小时 / 24 小时 / 自然月）。',
      hourly: { label: '1 小时', description: '滚动 1 小时窗口' },
      daily: { label: '24 小时', description: '滚动 24 小时窗口' },
      monthly: { label: '本月', description: '自然月' },
      unavailable: '暂时无法获取 Token 限额详情。',
    },
    snapshot: {
      title: '用量快照',
      description: '所选窗口的聚合指标。',
      period: '周期',
      periodFilterAria: '周期筛选',
      start: '起始',
      day: '日',
      week: '周',
      month: '月',
      requests: '请求数',
      success: '成功',
      errors: '错误',
      quotaExhausted: '限额',
      logsEmpty: '暂无日志。',
    },
    rotate: {
      title: '重置 Token Secret',
      description: '此操作会使当前 token secret 失效并生成新的 secret；4 位 Token ID 保持不变。客户端需要更新为新的 token。',
      cancel: '取消',
      confirm: '重置',
      inProgress: '重置中…',
    },
    rotated: {
      title: '新 Token 已生成',
      copiedDescription: '完整 token 已复制到剪贴板：',
      copyBlockedDescription: '自动复制被阻止；下方已选中完整 token，可手动复制。',
      close: '关闭',
      copy: '复制',
      copied: '已复制',
      copyFailed: '复制失败',
    },
    chart: {
      success: '成功',
      systemLimited: '系统限流',
      otherFailures: '其他失败',
      requestsAxis: '请求数',
      loading: '正在加载…',
    },
  },
}

export const adminDashboardProvidersTranslations: Record<Language, AdminDashboardProvidersTranslations> = {
  en: {
    title: 'Provider activity',
    requests: 'Requests',
    success: 'Success',
    errors: 'Errors',
    exhausted: 'Exhausted',
  },
  zh: {
    title: '提供方活动',
    requests: '请求数',
    success: '成功',
    errors: '错误',
    exhausted: '已耗尽',
  },
}
