import '../../test/happydom'

import { afterEach, beforeEach, describe, expect, it } from 'bun:test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { StrictMode, act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import { installDemoRuntime } from '../api/demo'
import { LanguageProvider } from '../i18n'
import { operationalClassGuidance } from '../lib/logOperationalClass'
import { ThemeProvider } from '../theme'
import { TooltipProvider } from '../components/ui/tooltip'
import AdminDashboard from './AdminDashboardRuntime'

/**
 * Plan todo 23b caller wiring proof for the shipped-but-unwired
 * AdminRecentRequestsPanel provider contract (commit 404941f6):
 * - providerOptions: an empty request page plus a facet-only provider proves the
 *   panel receives `/api/logs/catalog` facets (row-derived options stay empty);
 * - selectedProvider: a deep-linked `?provider=` value reaches the panel and the
 *   T21 `RequestLogsPageQuery.provider` on `/api/logs/list`.
 *
 * The Radix dropdown itself is intentionally not opened here: bun's shared
 * module cache evaluates `@radix-ui/react-use-layout-effect` before happy-dom in
 * some file orders, so portal mounting is not reliable across the full suite
 * (documented in .omo/evidence/task-23-serpapi-exa-brave-providers.txt). The
 * panel's `onProviderFilterChange` -> `?provider=` contract is covered by
 * AdminRecentRequestsPanel.providerFilter.test.tsx.
 */

const DEMO_STORAGE_KEY = 'tavily-hikari-demo-mode'
const originalFetch = window.fetch
const originalEventSource = window.EventSource
const originalMatchMedia = window.matchMedia
const originalRequestAnimationFrame = window.requestAnimationFrame
const originalCancelAnimationFrame = window.cancelAnimationFrame

let listUrls: string[] = []
let root: Root | null = null

interface MockMediaQueryList {
  matches: boolean
  media: string
  onchange: ((event: MediaQueryListEvent) => void) | null
  addListener: (listener: (event: MediaQueryListEvent) => void) => void
  removeListener: (listener: (event: MediaQueryListEvent) => void) => void
  addEventListener: (type: string, listener: (event: MediaQueryListEvent) => void) => void
  removeEventListener: (type: string, listener: (event: MediaQueryListEvent) => void) => void
  dispatchEvent: (event: Event) => boolean
}

function installViewportMocks(): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (query: string): MockMediaQueryList => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => true,
    }),
  })
  Object.defineProperty(window, 'requestAnimationFrame', {
    configurable: true,
    writable: true,
    value: (callback: FrameRequestCallback) => window.setTimeout(() => callback(Date.now()), 0),
  })
  Object.defineProperty(window, 'cancelAnimationFrame', {
    configurable: true,
    writable: true,
    value: (handle: number) => window.clearTimeout(handle),
  })
}

async function flushUi(ms = 0): Promise<void> {
  await act(async () => {
    await Promise.resolve()
    await Promise.resolve()
    await new Promise<void>((resolve) => window.setTimeout(resolve, 0))
    if (ms > 0) {
      await new Promise<void>((resolve) => window.setTimeout(resolve, ms))
    }
  })
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  })
}

async function waitFor(predicate: () => boolean, timeoutMs = 2500): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() <= deadline) {
    if (predicate()) return
    await flushUi(40)
  }
  throw new Error('Timed out waiting for the admin requests provider wiring')
}

function providerTrigger(): HTMLButtonElement | null {
  return document.querySelector<HTMLButtonElement>('button[aria-label^="Provider:"]')
}

async function mountRequestsRoute(search: string): Promise<HTMLElement> {
  window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
  window.history.replaceState(null, '', `/admin/requests${search}`)
  installDemoRuntime()

  const demoFetch = window.fetch.bind(window)
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url =
      typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url
    const response = await demoFetch(input, init)
    if (url.includes('/api/logs/list')) {
      listUrls.push(url)
      // Empty page: provider filter options can then only come from facets.
      const data = (await response.clone().json()) as { items?: Array<Record<string, unknown>> }
      return jsonResponse({ ...data, items: [] })
    }
    if (url.includes('/api/logs/catalog')) {
      const data = (await response.clone().json()) as { facets?: Record<string, unknown> }
      return jsonResponse({
        ...data,
        facets: {
          ...(data.facets ?? {}),
          providers: [
            { value: 'serpapi', count: 9 },
            { value: 'exa', count: 5 },
          ],
        },
      })
    }
    return response
  }

  const container = document.createElement('div')
  container.style.width = '1440px'
  container.style.minHeight = '1200px'
  document.body.appendChild(container)
  root = createRoot(container)

  await act(async () => {
    root!.render(
      <StrictMode>
        <LanguageProvider initialLanguage="en">
          <ThemeProvider>
            <TooltipProvider delayDuration={0} skipDelayDuration={0}>
              <AdminDashboard />
            </TooltipProvider>
          </ThemeProvider>
        </LanguageProvider>
      </StrictMode>,
    )
  })
  await flushUi(200)
  return container
}

/** Recursively drops every `providers` key so a payload can be pinned facet-free. */
function withoutProviders(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(withoutProviders)
  if (typeof value !== 'object' || value === null) return value
  const next: Record<string, unknown> = {}
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (key === 'providers') continue
    next[key] = withoutProviders(entry)
  }
  return next
}

/** The live `/api/summary` provider breakdown shape (orchestrator live-API evidence). */
const SUMMARY_PROVIDERS = [
  { provider: 'serpapi', total_requests: 1, success_count: 1, error_count: 0, quota_exhausted_count: 0 },
  { provider: 'tavily', total_requests: 1607, success_count: 1500, error_count: 100, quota_exhausted_count: 7 },
]

/**
 * Dashboard route reproducing the LIVE payload split: `/api/dashboard/overview`
 * carries `summary` and `summaryWindows` WITHOUT any `providers` array, while
 * `/api/summary` does carry it. The demo runtime alone hides this, because
 * `demoSummary()` is reused for both endpoints — so the overview payload is
 * stripped here and the summary payload is pinned to the live shape. The
 * Provider-activity card can therefore only render if the runtime reads
 * `fetchSummary()` rather than the overview's embedded summary (matrix M3).
 */
async function mountDashboardRoute(): Promise<void> {
  window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
  window.history.replaceState(null, '', '/admin')
  installDemoRuntime()

  const demoFetch = window.fetch.bind(window)
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url =
      typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url
    const response = await demoFetch(input, init)
    const parsed = new URL(url, 'http://localhost')
    if (parsed.pathname === '/api/dashboard/overview') {
      const data = (await response.clone().json()) as Record<string, unknown>
      return jsonResponse({
        ...data,
        summary: withoutProviders(data['summary']),
        summaryWindows: withoutProviders(data['summaryWindows']),
      })
    }
    if (parsed.pathname === '/api/summary') {
      const data = (await response.clone().json()) as Record<string, unknown>
      return jsonResponse({ ...data, providers: SUMMARY_PROVIDERS })
    }
    return response
  }

  const container = document.createElement('div')
  container.style.width = '1440px'
  container.style.minHeight = '1200px'
  document.body.appendChild(container)
  root = createRoot(container)

  await act(async () => {
    root!.render(
      <StrictMode>
        <LanguageProvider initialLanguage="en">
          <ThemeProvider>
            <TooltipProvider delayDuration={0} skipDelayDuration={0}>
              <AdminDashboard />
            </TooltipProvider>
          </ThemeProvider>
        </LanguageProvider>
      </StrictMode>,
    )
  })
  await flushUi(300)
}

beforeEach(() => {
  listUrls = []
  installViewportMocks()
  HTMLElement.prototype.scrollIntoView = () => undefined
})

afterEach(async () => {
  if (root) {
    await act(async () => {
      root!.unmount()
    })
    root = null
  }
  window.fetch = originalFetch
  Object.defineProperty(window, 'EventSource', {
    configurable: true,
    writable: true,
    value: originalEventSource,
  })
  window.matchMedia = originalMatchMedia
  window.requestAnimationFrame = originalRequestAnimationFrame
  window.cancelAnimationFrame = originalCancelAnimationFrame
  window.localStorage.removeItem(DEMO_STORAGE_KEY)
  delete document.documentElement.dataset.demoMode
  delete window.__tavilyHikariDemoFetch
  delete window.__tavilyHikariDemoEventSource
  delete window.__tavilyHikariDemoInstalled
  document.body.innerHTML = ''
})

describe('AdminDashboardRuntime provider caller wiring (plan todo 23b)', () => {
  it('passes facet providers into the panel when the request page has no provider rows', async () => {
    await mountRequestsRoute('')

    // With an empty request page, row-derived options are empty; the provider
    // facet field can only render because providerOptions came from the
    // `/api/logs/catalog` facet payload.
    await waitFor(() => providerTrigger() !== null)
    expect(providerTrigger()?.getAttribute('aria-label')).toContain('Provider: All providers')
    expect(listUrls.some((url) => url.includes('provider='))).toBe(false)
  }, 20_000)

  it('forwards the provider selection into the T21 ?provider= list query', async () => {
    await mountRequestsRoute('?provider=exa')

    await waitFor(() => providerTrigger() !== null)
    await waitFor(() => listUrls.some((url) => url.includes('provider=exa')))

    // `selectedProvider` reaches the panel control ...
    expect(providerTrigger()?.getAttribute('aria-label')).toContain('Provider: Exa')
    // ... and the T21 query value is emitted on the request-log list request.
    const filteredUrl = listUrls.find((url) => url.includes('provider=exa'))
    expect(filteredUrl).toBeDefined()
    expect(new URLSearchParams(filteredUrl!.split('?')[1] ?? '').get('provider')).toBe('exa')
  }, 20_000)
})

describe('AdminDashboardRuntime summary provider wiring (plan todo 10 / matrix M3)', () => {
  it('renders the provider activity card from /api/summary, not the overview summary', async () => {
    await mountDashboardRoute()

    // Given the overview payload carries no provider breakdown at all, the card
    // can only appear if the runtime fed DashboardOverview from fetchSummary().
    await waitFor(() => document.querySelector('.dashboard-provider-panel') !== null)
    const rows = Array.from(
      document.querySelectorAll<HTMLElement>('.dashboard-provider-panel tbody tr[data-provider]'),
    )
    expect(rows.map((row) => row.dataset.provider)).toEqual(['serpapi', 'tavily'])

    // And the rendered counts are the /api/summary values, label included.
    const serpapi = rows.find((row) => row.dataset.provider === 'serpapi')
    if (serpapi === undefined) {
      throw new Error('The provider activity card rendered no serpapi row')
    }
    const cells = Array.from(serpapi.querySelectorAll('td')).map((cell) => (cell.textContent ?? '').trim())
    expect(cells).toEqual(['SerpApi', '1', '1', '0', '0'])
  }, 20_000)
})

describe('AdminDashboardRuntime failure-guidance provider wiring (plan todo 10 / matrix M6)', () => {
  const runtimeSourcePath = fileURLToPath(new URL('./AdminDashboardRuntime.tsx', import.meta.url))

  /**
   * `LogRow`/`LogDetails` — this module's only `operationalClassGuidance`
   * consumer — are referenced by no route: the live admin requests surface is
   * `AdminRecentRequestsPanel`, which renders no failure guidance at all. There
   * is therefore no mounted surface to assert on, and the wiring is pinned
   * structurally instead. The resolver's 4th parameter is OPTIONAL, so a
   * dropped argument still compiles and silently degrades to generic upstream
   * copy — precisely the regression this guard catches.
   */
  function guidanceCallArgumentLists(): string[][] {
    const source = readFileSync(runtimeSourcePath, 'utf8')
    return [...source.matchAll(/operationalClassGuidance\(([^)]*)\)/g)].map((match) =>
      (match[1] ?? '').split(',').map((argument) => argument.trim()),
    )
  }

  it('passes the log row provider as the 4th guidance argument at every call site', () => {
    const callSites = guidanceCallArgumentLists()
    expect(callSites.length).toBeGreaterThan(0)
    for (const args of callSites) {
      expect(args).toHaveLength(4)
      expect(args[3]).toBe('log.provider')
    }
  })

  it('resolves provider-named guidance for a SerpApi 429 row and degrades without the argument', () => {
    // Given the argument list the call site passes for a serpapi row
    const serpapi = operationalClassGuidance('upstream_error', 'upstream_rate_limited_429', 'en', 'serpapi')
    // Then the copy names SerpApi and never Tavily
    expect(serpapi).toContain('SerpApi')
    expect(serpapi).not.toContain('Tavily')

    // And the 4th argument is load-bearing: omitting it yields generic copy.
    const generic = operationalClassGuidance('upstream_error', 'upstream_rate_limited_429', 'en')
    expect(generic).not.toContain('SerpApi')
    expect(generic).not.toBe(serpapi)
  })
})