import '../../test/happydom'

import { afterEach, beforeEach, describe, expect, it } from 'bun:test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { StrictMode, act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import { installDemoRuntime } from '../api/demo'
import { fetchDashboardOverview, type DashboardSnapshotEvent } from '../api'
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
let requestedPaths: string[] = []
let root: Root | null = null

/**
 * Minimal controlled `EventSource`: the admin runtime's `/api/events` snapshot
 * handler is driven on demand so the SSE ordering regression is deterministic
 * (same shape as `AdminDashboardRuntime.route-switch.test.tsx`).
 */
class ControlledEventSource {
  static instances: ControlledEventSource[] = []

  readonly url: string
  readonly withCredentials = false
  onopen: ((this: EventSource, event: Event) => unknown) | null = null
  onerror: ((this: EventSource, event: Event) => unknown) | null = null
  private readonly listeners = new Map<string, Set<EventListenerOrEventListenerObject>>()

  constructor(url: string | URL) {
    this.url = String(url)
    ControlledEventSource.instances.push(this)
    queueMicrotask(() => this.onopen?.call(this as unknown as EventSource, new Event('open')))
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    if (!listener) return
    const listeners = this.listeners.get(type) ?? new Set<EventListenerOrEventListenerObject>()
    listeners.add(listener)
    this.listeners.set(type, listeners)
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    if (listener) this.listeners.get(type)?.delete(listener)
  }

  close(): void {
    this.listeners.clear()
  }

  emitSnapshot(payload: DashboardSnapshotEvent): void {
    const event = new MessageEvent('snapshot', { data: JSON.stringify(payload) })
    for (const listener of this.listeners.get('snapshot') ?? []) {
      if (typeof listener === 'function') {
        listener.call(this as unknown as EventSource, event)
      } else {
        listener.handleEvent(event)
      }
    }
  }
}

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

/**
 * Contract C2: `/api/dashboard/overview` — and the SSE snapshot built by the same
 * payload builder — carries `summary.providers`, so the Provider-activity card
 * must read it from the overview state. `/api/summary` is pinned to a *different*
 * breakdown: an Exa row on screen means the removed dedicated fetch came back.
 */
const OVERVIEW_PROVIDERS = [
  { provider: 'serpapi', total_requests: 41, success_count: 39, error_count: 2, quota_exhausted_count: 0 },
  { provider: 'tavily', total_requests: 1607, success_count: 1500, error_count: 100, quota_exhausted_count: 7 },
]

/** Decoy breakdown: only the retired `/api/summary` path would ever render it. */
const SUMMARY_PROVIDERS = [
  { provider: 'exa', total_requests: 999, success_count: 999, error_count: 0, quota_exhausted_count: 0 },
]

/** A third breakdown, so an SSE snapshot is distinguishable from both pins. */
const SSE_PROVIDERS = [
  { provider: 'brave', total_requests: 42, success_count: 40, error_count: 2, quota_exhausted_count: 0 },
]

/**
 * Dashboard route with both provider sources pinned: the overview summary carries
 * `OVERVIEW_PROVIDERS` while `/api/summary` answers the decoy. `eventSource`
 * swaps in {@link ControlledEventSource} so a snapshot can be emitted on demand.
 */
async function mountDashboardRoute(eventSource?: typeof EventSource): Promise<void> {
  window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
  window.history.replaceState(null, '', '/admin')
  installDemoRuntime()
  if (eventSource) {
    Object.defineProperty(window, 'EventSource', {
      configurable: true,
      writable: true,
      value: eventSource,
    })
  }

  const demoFetch = window.fetch.bind(window)
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url =
      typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url
    const parsed = new URL(url, 'http://localhost')
    requestedPaths.push(parsed.pathname)
    const response = await demoFetch(input, init)
    if (parsed.pathname === '/api/dashboard/overview') {
      const data = (await response.clone().json()) as Record<string, unknown>
      const summary = (data['summary'] ?? {}) as Record<string, unknown>
      return jsonResponse({ ...data, summary: { ...summary, providers: OVERVIEW_PROVIDERS } })
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

function providerCardRows(): HTMLElement[] {
  return Array.from(
    document.querySelectorAll<HTMLElement>('.dashboard-provider-panel tbody tr[data-provider]'),
  )
}

function rowCells(row: HTMLElement): string[] {
  return Array.from(row.querySelectorAll('td')).map((cell) => (cell.textContent ?? '').trim())
}

beforeEach(() => {
  listUrls = []
  requestedPaths = []
  ControlledEventSource.instances = []
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

describe('AdminDashboardRuntime summary provider wiring (contract C2)', () => {
  it('renders the provider activity card from the overview payload, not /api/summary', async () => {
    // The demo EventSource is controlled so no unsolicited snapshot overwrites
    // the pinned overview payload between the fetch and the assertion.
    await mountDashboardRoute(ControlledEventSource as unknown as typeof EventSource)

    // Given the overview summary carries SerpApi+Tavily and /api/summary answers
    // an Exa decoy, the card can only show these rows if it reads the overview.
    await waitFor(() => document.querySelector('.dashboard-provider-panel') !== null)
    const rows = providerCardRows()
    expect(rows.map((row) => row.dataset.provider)).toEqual(['serpapi', 'tavily'])

    const serpapi = rows.find((row) => row.dataset.provider === 'serpapi')
    if (serpapi === undefined) {
      throw new Error('The provider activity card rendered no serpapi row')
    }
    expect(rowCells(serpapi)).toEqual(['SerpApi', '41', '39', '2', '0'])

    // And the retired dedicated request never happens: the card rides the
    // overview fetch that the dashboard route already makes.
    expect(requestedPaths).not.toContain('/api/summary')
    expect(requestedPaths).toContain('/api/dashboard/overview')
  }, 20_000)

  it('keeps the card fresh when an SSE snapshot carries a new provider breakdown', async () => {
    await mountDashboardRoute(ControlledEventSource as unknown as typeof EventSource)
    await waitFor(() => providerCardRows().length === 2)
    expect(providerCardRows().map((row) => row.dataset.provider)).toEqual(['serpapi', 'tavily'])

    const eventSource = ControlledEventSource.instances
      .filter((source) => source.url === '/api/events')
      .at(-1)
    expect(eventSource).toBeDefined()
    const overview = await fetchDashboardOverview()

    // When the shared SSE snapshot arrives with its own summary.providers
    await act(async () => {
      eventSource?.emitSnapshot({
        ...overview,
        keys: overview.exhaustedKeys,
        logs: overview.recentLogs,
        summary: { ...overview.summary, providers: SSE_PROVIDERS },
      })
    })

    // Then the card follows the snapshot instead of keeping the pre-SSE rows:
    // the retired fetchSummary state could not be bumped by a snapshot, which is
    // the ordering hazard the 2024b562 guard existed for.
    await waitFor(() => providerCardRows().map((row) => row.dataset.provider).join(',') === 'brave')
    const brave = providerCardRows()[0]
    if (brave === undefined) {
      throw new Error('The SSE snapshot rendered no provider row')
    }
    expect(rowCells(brave)).toEqual(['Brave', '42', '40', '2', '0'])
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