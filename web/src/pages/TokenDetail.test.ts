import '../../test/happydom'

import { afterEach, describe, expect, it } from 'bun:test'
import { act, createElement, type ReactElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { renderToStaticMarkup } from 'react-dom/server'

import { TooltipProvider } from '../components/ui/tooltip'
import { LanguageProvider, type Language } from '../i18n'
import { ThemeProvider } from '../theme'
import TokenDetail, { __testables } from './TokenDetail'

/**
 * Plan todo 8 / matrix I1: the token detail page renders the
 * `admin.tokenDetail.*` catalog instead of hardcoded English, in both
 * languages, and a missing catalog entry can never surface as a raw key path.
 *
 * Plan todo 14 closed the remaining catalog gap: QuotaStatCard's reset copy
 * (`admin.tokenDetail.reset.*`) and the operator-facing failure copy
 * (`admin.tokenDetail.errors.*`) are dictionary leaves too, so both directions
 * are asserted for the reset label, the upcoming-reset line, and the
 * detail-load failure banner.
 */

const TOKEN_ID = 'a1b2'
const RAW_KEY_PATH_MARKER = 'admin.tokenDetail.'
const UNEXPECTED_REQUEST_MARKER = 'unexpected test request'
const DETAIL_FAILURE_MARKER = 'stub-detail-unavailable'
// A non-Error rejection is what makes the catch fall back to the catalog string
// instead of `e.message`, so this marker must never reach the DOM.
const NON_ERROR_FAILURE = 'stub-non-error-failure'
const UPCOMING_RESET_AT = Math.floor(Date.now() / 1000) + 3600

type DetailStub = 'ready' | 'failing' | 'rejecting' | 'upcoming-reset'

const detailPayload = {
  id: TOKEN_ID,
  enabled: true,
  note: 'primary token',
  owner: { userId: 'usr_alice', displayName: 'Alice Chen', username: 'alice' },
  total_requests: 9241,
  created_at: 1_762_100_200,
  last_used_at: 1_762_390_010,
  quota_state: 'normal',
  quota_hourly_used: 33,
  quota_hourly_limit: 100,
  quota_daily_used: 192,
  quota_daily_limit: 500,
  quota_monthly_used: 2511,
  quota_monthly_limit: 5000,
  quota_hourly_reset_at: null,
  quota_daily_reset_at: null,
  quota_monthly_reset_at: null,
}

const metricsPayload = {
  total_requests: 9241,
  success_count: 9003,
  error_count: 175,
  quota_exhausted_count: 63,
  last_activity: 1_762_390_010,
}

const logsListPayload = {
  items: [],
  pageSize: 20,
  nextCursor: null,
  prevCursor: null,
  hasOlder: false,
  hasNewer: false,
}

const logsCatalogPayload = {
  retentionDays: 90,
  requestKindOptions: [],
  facets: { results: [], keyEffects: [], bindingEffects: [], selectionEffects: [], tokens: [], keys: [] },
}

const detailPayloadWithUpcomingReset = { ...detailPayload, quota_hourly_reset_at: UPCOMING_RESET_AT }

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  })
}

class StubEventSource {
  static CONNECTING = 0
  static OPEN = 1
  static CLOSED = 2
  readonly url: string
  readyState = StubEventSource.OPEN
  onopen: ((event: Event) => unknown) | null = null
  onerror: ((event: Event) => unknown) | null = null

  constructor(url: string) {
    this.url = url
  }

  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {
    this.readyState = StubEventSource.CLOSED
  }
}

const originalFetch = window.fetch
const originalEventSource = window.EventSource
let root: Root | null = null

function defineWindowGlobal(name: 'EventSource' | 'fetch', value: unknown): void {
  // Happy DOM shares globalThis with every test file in the run, and a sibling
  // suite defines EventSource as a non-writable property, so a plain assignment
  // throws once that suite has loaded.
  Object.defineProperty(window, name, {
    configurable: true,
    writable: true,
    value,
  })
}

function setEventSource(ctor: unknown): void {
  defineWindowGlobal('EventSource', ctor)
}

function setFetch(value: unknown): void {
  defineWindowGlobal('fetch', value)
}

function installTokenApiStub(detail: DetailStub): void {
  setEventSource(StubEventSource)
  setFetch(async (input: RequestInfo | URL): Promise<Response> => {
    const request = input instanceof Request ? input : new Request(input)
    const { pathname } = new URL(request.url, window.location.origin)
    const base = `/api/tokens/${TOKEN_ID}`
    if (pathname === base) {
      if (detail === 'rejecting') throw NON_ERROR_FAILURE
      if (detail === 'upcoming-reset') return jsonResponse(detailPayloadWithUpcomingReset)
      return detail === 'failing'
        ? new Response(JSON.stringify({ error: DETAIL_FAILURE_MARKER }), {
            status: 500,
            headers: { 'content-type': 'application/json' },
          })
        : jsonResponse(detailPayload)
    }
    // In the rejecting mode the log list stays pending on purpose: its success
    // path clears the error banner the detail failure just set, which would make
    // the assertion depend on request resolution order.
    if (detail === 'rejecting' && pathname === `${base}/logs/list`) {
      return new Promise<Response>(() => undefined)
    }
    if (pathname === `${base}/metrics`) return jsonResponse(metricsPayload)
    if (pathname === `${base}/logs/list`) return jsonResponse(logsListPayload)
    if (pathname === `${base}/logs/catalog`) return jsonResponse(logsCatalogPayload)
    if (pathname === `${base}/secret/rotate`) return jsonResponse({ token: 'th-a1b2-rotated-secret' })
    // The usage series stays pending on purpose: the charts keep their loading
    // placeholder, which is the localized string under test, and Chart.js never
    // measures a canvas that Happy DOM does not model.
    if (pathname === `${base}/metrics/usage-series`) return new Promise<Response>(() => undefined)
    return new Response(JSON.stringify({ error: `${UNEXPECTED_REQUEST_MARKER}: ${pathname}` }), {
      status: 404,
      headers: { 'content-type': 'application/json' },
    })
  })
}

function tokenDetailTree(language: Language): ReactElement {
  return createElement(
    LanguageProvider,
    { initialLanguage: language },
    createElement(
      ThemeProvider,
      null,
      createElement(TooltipProvider, null, createElement(TokenDetail, { id: TOKEN_ID })),
    ),
  )
}

function renderStaticMarkup(language: Language): string {
  return renderToStaticMarkup(tokenDetailTree(language))
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve()
    await Promise.resolve()
    await new Promise<void>((resolve) => window.setTimeout(resolve, 0))
  })
}

async function waitFor(predicate: () => boolean, label: string, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() <= deadline) {
    if (predicate()) return
    await flush()
  }
  throw new Error(`${label}: timed out after ${timeoutMs}ms`)
}

async function mountTokenDetail(
  language: Language,
  detail: DetailStub = 'ready',
): Promise<HTMLDivElement> {
  installTokenApiStub(detail)
  const container = document.createElement('div')
  document.body.appendChild(container)
  const mounted = createRoot(container)
  root = mounted
  await act(async () => {
    mounted.render(tokenDetailTree(language))
  })
  await flush()
  return container
}

// Radix portals the dialog outside the mounted root, so a dialog-interaction
// assertion is not viable in a full `bun test` run: sibling suites leak global
// DOM state that stops React from mounting the portal (see
// .omo/evidence/ui-alignment/t8-tokendetail.log, finding F-2). The rotate and
// rotated dialog copy is consumed from `admin.tokenDetail.rotate|rotated` in
// TokenDetail.tsx and covered by the literal-leak audit instead.

afterEach(async () => {
  const mounted = root
  root = null
  if (mounted) {
    await act(async () => mounted.unmount())
  }
  document.body.innerHTML = ''
  setFetch(originalFetch)
  setEventSource(originalEventSource)
})

describe('TokenDetail request log pagination helpers', () => {
  it('changes the list query key when the page size changes on the newest page', () => {
    expect(__testables.buildTokenLogsListQueryKey('token:a1b2', null, 'older', 20)).toBe(
      'token:a1b2:cursor=:direction=older:perPage=20',
    )
    expect(__testables.buildTokenLogsListQueryKey('token:a1b2', null, 'older', 50)).toBe(
      'token:a1b2:cursor=:direction=older:perPage=50',
    )
  })

  it('clears stale pagination controls with an empty page shell for the requested page size', () => {
    expect(__testables.createEmptyTokenLogsListPage(50)).toEqual({
      items: [],
      pageSize: 50,
      nextCursor: null,
      prevCursor: null,
      hasOlder: false,
      hasNewer: false,
    })
  })
})

describe('TokenDetail localization', () => {
  it('renders the zh catalog copy on the data-absent surface', () => {
    const markup = renderStaticMarkup('zh')

    for (const localized of [
      '访问令牌详情',
      'title="通过 SSE 实时更新"',
      'aria-label="重置 Secret"',
      '>返回<',
      '快速统计',
      '滚动用量窗口（1 小时 / 24 小时 / 自然月）。',
      '用量快照',
      '所选窗口的聚合指标。',
      'aria-label="周期筛选"',
      '>周期<',
      '>起始<',
      '正在加载…',
    ]) {
      expect(markup).toContain(localized)
    }
    for (const hardcoded of [
      'Access Token Detail',
      'Live updates via SSE',
      'Regenerate Secret',
      '>Back<',
      'Quick Stats',
      'Usage Snapshot',
      'Period filter',
      'Loading…',
    ]) {
      expect(markup).not.toContain(hardcoded)
    }
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders the zh unavailable copy when the token detail request fails', async () => {
    const mounted = await mountTokenDetail('zh', 'failing')
    await waitFor(() => mounted.innerHTML.includes('暂时无法获取 Token 详情。'), 'zh unavailable copy')

    const markup = mounted.innerHTML
    expect(markup).toContain('暂时无法获取 Token 详情。')
    expect(markup).toContain('暂时无法获取 Token 限额详情。')
    expect(markup).not.toContain('Token details are unavailable right now.')
    expect(markup).not.toContain('Token quota details are unavailable right now.')
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders localized info, quota, and snapshot labels once the zh data loads', async () => {
    const mounted = await mountTokenDetail('zh')
    await waitFor(() => mounted.innerHTML.includes('aria-label="Token 元数据"'), 'zh info grid')

    const markup = mounted.innerHTML
    for (const localized of [
      '访问令牌详情',
      'title="通过 SSE 实时更新"',
      'aria-label="重置 Secret"',
      '>返回<',
      'aria-label="Token 元数据"',
      '>状态<',
      '>已启用<',
      '>总请求数<',
      '>创建时间<',
      '>上次使用<',
      '>备注<',
      '快速统计',
      '滚动用量窗口（1 小时 / 24 小时 / 自然月）。',
      '>1 小时<',
      '>24 小时<',
      '>本月<',
      '>滚动 1 小时窗口<',
      '>滚动 24 小时窗口<',
      '>自然月<',
      '用量快照',
      '所选窗口的聚合指标。',
      'aria-label="周期筛选"',
      '>周期<',
      '>起始<',
      '>请求数<',
      '>成功<',
      '>错误<',
      '>限额<',
      '暂无日志。',
      '正在加载…',
      '>尚未使用<',
    ]) {
      expect(markup).toContain(localized)
    }
    for (const hardcoded of [
      'Access Token Detail',
      '>Not used yet<',
      'Live updates via SSE',
      'Regenerate Secret',
      '>Back<',
      'Token metadata',
      '>Status<',
      '>Enabled<',
      'Total Requests',
      '>Created<',
      'Last Used',
      '>Note<',
      'Quick Stats',
      '>1 Hour<',
      '>24 Hours<',
      'This Month',
      'Rolling 1-hour window',
      'Rolling 24-hour window',
      'Calendar month',
      'Usage Snapshot',
      'Period filter',
      '>Requests<',
      '>Success<',
      '>Errors<',
      'Quota Exhausted',
      'No logs yet.',
      'Loading…',
      UNEXPECTED_REQUEST_MARKER,
    ]) {
      expect(markup).not.toContain(hardcoded)
    }
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('keeps the en labels on the loaded surface', async () => {
    const mounted = await mountTokenDetail('en')
    await waitFor(() => mounted.innerHTML.includes('aria-label="Token metadata"'), 'en info grid')

    const markup = mounted.innerHTML
    for (const original of [
      'Access Token Detail',
      'title="Live updates via SSE"',
      'aria-label="Regenerate Secret"',
      '>Regenerate Secret<',
      '>Back<',
      'aria-label="Token metadata"',
      '>Token ID<',
      '>Status<',
      '>Enabled<',
      '>Total Requests<',
      '>Created<',
      '>Last Used<',
      '>Note<',
      'Quick Stats',
      'Rolling usage windows (1 hour / 24 hours / calendar month).',
      '>1 Hour<',
      '>24 Hours<',
      '>This Month<',
      '>Rolling 1-hour window<',
      'Usage Snapshot',
      'Aggregated metrics for the selected window.',
      'aria-label="Period filter"',
      '>Period<',
      '>Start<',
      '>Requests<',
      '>Success<',
      '>Errors<',
      '>Quota Exhausted<',
      'No logs yet.',
      'Loading…',
      '>Not used yet<',
    ]) {
      expect(markup).toContain(original)
    }
    for (const localized of ['访问令牌详情', '总请求数', '暂无日志。', '正在加载…', '尚未使用']) {
      expect(markup).not.toContain(localized)
    }
    expect(markup).not.toContain(UNEXPECTED_REQUEST_MARKER)
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders the zh upcoming-reset copy when the quota reset time is in the future', async () => {
    const mounted = await mountTokenDetail('zh', 'upcoming-reset')
    await waitFor(() => mounted.innerHTML.includes('下次重置：'), 'zh upcoming reset copy')

    const markup = mounted.innerHTML
    expect(markup).toContain('下次重置：')
    expect(markup).toContain('>尚未使用<')
    expect(markup).not.toContain('Next reset:')
    expect(markup).not.toContain('>Not used yet<')
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders the en upcoming-reset copy when the quota reset time is in the future', async () => {
    const mounted = await mountTokenDetail('en', 'upcoming-reset')
    await waitFor(() => mounted.innerHTML.includes('Next reset:'), 'en upcoming reset copy')

    const markup = mounted.innerHTML
    expect(markup).toContain('Next reset:')
    expect(markup).toContain('>Not used yet<')
    expect(markup).not.toContain('下次重置：')
    expect(markup).not.toContain('尚未使用')
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders the zh detail-load failure copy when the request fails without an Error', async () => {
    const mounted = await mountTokenDetail('zh', 'rejecting')
    await waitFor(() => mounted.innerHTML.includes('加载 Token 详情失败'), 'zh load failure copy')

    const markup = mounted.innerHTML
    expect(markup).toContain('加载 Token 详情失败')
    expect(markup).not.toContain('Failed to load token details')
    expect(markup).not.toContain(NON_ERROR_FAILURE)
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })

  it('renders the en detail-load failure copy when the request fails without an Error', async () => {
    const mounted = await mountTokenDetail('en', 'rejecting')
    await waitFor(() => mounted.innerHTML.includes('Failed to load token details'), 'en load failure copy')

    const markup = mounted.innerHTML
    expect(markup).toContain('Failed to load token details')
    expect(markup).not.toContain('加载 Token 详情失败')
    expect(markup).not.toContain(NON_ERROR_FAILURE)
    expect(markup).not.toContain(RAW_KEY_PATH_MARKER)
  })
})
