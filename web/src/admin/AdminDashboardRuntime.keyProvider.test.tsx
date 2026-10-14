import '../../test/happydom'

import { afterEach, beforeEach, describe, expect, it } from 'bun:test'
import { StrictMode, act } from 'react'
import { createRoot, type Root } from 'react-dom/client'

import { installDemoRuntime } from '../api/demo'
import { LanguageProvider } from '../i18n'
import { ThemeProvider } from '../theme'
import { TooltipProvider } from '../components/ui/tooltip'
import AdminDashboard from './AdminDashboardRuntime'

/**
 * Plan todo 22 caller wiring proof: the admin key table renders a provider
 * column with text badges (legacy NULL provider -> Tavily), and the provider
 * filter narrows the rendered rows through the engine-side `?provider=` query.
 */

const DEMO_STORAGE_KEY = 'tavily-hikari-demo-mode'
const originalFetch = window.fetch
const originalEventSource = window.EventSource
const originalMatchMedia = window.matchMedia
const originalRequestAnimationFrame = window.requestAnimationFrame
const originalCancelAnimationFrame = window.cancelAnimationFrame

const KEY_ROWS = [
  {
    id: 'legacy1',
    provider: undefined,
    status: 'active',
    group: 'default',
    registration_ip: null,
    registration_region: null,
    status_changed_at: null,
    last_used_at: null,
    deleted_at: null,
    quota_limit: 100,
    quota_remaining: 50,
    quota_synced_at: null,
    total_requests: 1,
    success_count: 1,
    error_count: 0,
    quota_exhausted_count: 0,
    quarantine: null,
    transient_backoff: null,
  },
  { id: 'serp1', provider: 'serpapi' },
  { id: 'exa1', provider: 'exa' },
  { id: 'brave1', provider: 'brave' },
]

let keysUrls: string[] = []
let root: Root | null = null

/** Extra `/api/keys` facet arrays the mock should serve (plan todo 10 / matrix M12). */
interface KeysFacetOverrides {
  providers?: Array<{ value: string; count: number }>
}

function installViewportMocks(): void {
  Object.defineProperty(window, 'matchMedia', {
    configurable: true,
    writable: true,
    value: (query: string) => ({
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

async function waitFor(predicate: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() <= deadline) {
    if (predicate()) return
    await flushUi(40)
  }
  throw new Error('Timed out waiting for the admin key provider wiring')
}

function badges(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-provider].key-provider-badge'))
}

/**
 * happy-dom cannot deliver `input`/`change` DOM events to React's
 * ChangeEventPlugin in this suite: dispatching one on a text input throws
 * `TypeError: null is not an object (evaluating 'inst.tag')` inside
 * `getInstIfValueChanged`, because React resolves no target instance for the
 * node. Native `<select>` change events do work (see the todo 22 filter test),
 * but the batch textarea does not, so the overlay is driven through the React
 * `onChange` prop the element was actually rendered with. Everything downstream
 * — `setNewKeysText`, the `keysBatchParsed` memo, the parse call, the rendered
 * footer — is the real component state machine.
 */
let reactKeySuffix: string | null = null

/**
 * React's expando keys are hidden from `ownKeys` on happy-dom's
 * HTMLSelectElement (named-property proxy), so the per-module suffix is
 * discovered once from any element that does expose it and then reused.
 */
function discoverReactKeySuffix(): string {
  if (reactKeySuffix !== null) return reactKeySuffix
  for (const node of Array.from(document.querySelectorAll('*'))) {
    const key = Object.getOwnPropertyNames(node).find((name) => name.startsWith('__reactFiber'))
    if (key !== undefined) {
      reactKeySuffix = key.slice('__reactFiber'.length)
      return reactKeySuffix
    }
  }
  throw new Error('No React fiber key is discoverable in the document')
}

function reactPropsOf(element: HTMLElement): Record<string, unknown> {
  const props = (element as unknown as Record<string, unknown>)[`__reactProps${discoverReactKeySuffix()}`]
  if (typeof props !== 'object' || props === null) {
    throw new Error(`No React props on <${element.nodeName.toLowerCase()}>`)
  }
  return props as Record<string, unknown>
}

interface ReactChangeEvent {
  target: { value: string }
  currentTarget: { value: string }
}

function fireReactChange(element: HTMLElement, value: string): void {
  const onChange = reactPropsOf(element)['onChange']
  if (typeof onChange !== 'function') {
    throw new Error(`No React onChange on <${element.nodeName.toLowerCase()}>`)
  }
  const handler = onChange as (event: ReactChangeEvent) => void
  handler({ target: { value }, currentTarget: { value } })
}

/** Same delivery path as `fireReactChange`, for the handlers that ignore the event. */
function fireReactClick(element: HTMLElement): void {
  const onClick = reactPropsOf(element)['onClick']
  if (typeof onClick !== 'function') {
    throw new Error(`No React onClick on <${element.nodeName.toLowerCase()}>`)
  }
  const handler = onClick as (event: { preventDefault: () => void; stopPropagation: () => void }) => void
  handler({ preventDefault: () => undefined, stopPropagation: () => undefined })
}

function batchOverlay(): HTMLElement | null {
  return document.querySelector<HTMLElement>('.keys-batch-overlay')
}

/** The overlay footer holds exactly one button: the batch Add Key action. */
function batchAddButton(overlay: HTMLElement): HTMLButtonElement {
  const addButton = overlay.querySelector<HTMLButtonElement>('button')
  if (addButton === null) {
    throw new Error('Missing the batch overlay Add button')
  }
  if (overlay.querySelectorAll('button').length !== 1) {
    throw new Error('The batch overlay gained an unexpected extra button')
  }
  return addButton
}

function batchProviderSelect(overlay: HTMLElement): HTMLSelectElement {
  const select = overlay.querySelector<HTMLSelectElement>('.key-provider-select select')
  if (select === null) {
    throw new Error('Missing the batch overlay provider select')
  }
  return select
}

function batchTextarea(overlay: HTMLElement): HTMLTextAreaElement {
  const textarea = overlay.querySelector<HTMLTextAreaElement>('textarea')
  if (textarea === null) {
    throw new Error('Missing the batch overlay textarea')
  }
  return textarea
}

/** Focusing the collapsed quick-add input is the overlay's real open affordance. */
async function openBatchOverlay(): Promise<HTMLElement> {
  const collapsed = document.querySelector<HTMLInputElement>('input[name="collapsed-key-input"]')
  if (collapsed === null) {
    throw new Error('Missing the collapsed quick-add key input')
  }
  await act(async () => {
    collapsed.focus()
    collapsed.dispatchEvent(new FocusEvent('focusin', { bubbles: true }))
  })
  await waitFor(() => batchOverlay() !== null)
  const overlay = batchOverlay()
  if (overlay === null) {
    throw new Error('The batch overlay never mounted')
  }
  return overlay
}

/** The table filter select, told apart from the batch select by its `all` option. */
function providerFilterSelect(): HTMLSelectElement | null {
  const selects = Array.from(document.querySelectorAll<HTMLSelectElement>('select[aria-label="Provider"]'))
  return selects.find((select) => select.querySelector('option[value="all"]') !== null) ?? null
}

function providerOptionText(select: HTMLSelectElement, value: string): string {
  const option = select.querySelector<HTMLOptionElement>(`option[value="${value}"]`)
  if (option === null) {
    throw new Error(`Missing the provider filter option: ${value}`)
  }
  return (option.textContent ?? '').trim()
}

async function mountKeysRoute(facetOverrides?: KeysFacetOverrides): Promise<void> {
  window.localStorage.setItem(DEMO_STORAGE_KEY, 'true')
  window.history.replaceState(null, '', '/admin/keys')
  installDemoRuntime()

  const demoFetch = window.fetch.bind(window)
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.toString() : input.url
    const parsed = new URL(url, 'http://localhost')
    if (parsed.pathname === '/api/keys' && (init?.method ?? 'GET') === 'GET') {
      keysUrls.push(url)
      const provider = parsed.searchParams.get('provider')
      const items = KEY_ROWS.filter((row) => !provider || row.provider === provider).map((row) => {
        const base = KEY_ROWS[0]!
        return {
          ...base,
          ...row,
          provider: row.provider ?? 'tavily',
        }
      })
      return jsonResponse({
        items,
        total: items.length,
        page: 1,
        per_page: 20,
        facets: { groups: [], statuses: [], regions: [], ...facetOverrides },
      })
    }
    return demoFetch(input, init)
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
  await flushUi(250)
}

beforeEach(() => {
  keysUrls = []
  reactKeySuffix = null
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

describe('AdminDashboard key provider table + filter (plan todo 22)', () => {
  it('renders a provider column with text badges including legacy NULL -> Tavily', async () => {
    await mountKeysRoute()
    await waitFor(() => badges().length >= 4)

    const found = badges().map((badge) => badge.dataset.provider)
    expect(found).toEqual(expect.arrayContaining(['tavily', 'serpapi', 'exa', 'brave']))
    const labels = badges().map((badge) => badge.textContent)
    expect(labels).toEqual(expect.arrayContaining(['Tavily', 'SerpApi', 'Exa', 'Brave']))
    expect(document.body.textContent).toContain('Provider')
  }, 20_000)

  it("narrows the table to Brave rows when the provider filter selects 'Brave'", async () => {
    await mountKeysRoute()
    await waitFor(() => badges().length >= 4)

    const select = document.querySelector<HTMLSelectElement>('select[aria-label="Provider"]')
    expect(select).not.toBeNull()

    await act(async () => {
      select!.value = 'brave'
      select!.dispatchEvent(new Event('change', { bubbles: true }))
    })

    await waitFor(() => keysUrls.some((url) => url.includes('provider=brave')))
    await waitFor(() => badges().length > 0 && badges().every((badge) => badge.dataset.provider === 'brave'))

    expect(new Set(badges().map((badge) => badge.dataset.provider))).toEqual(new Set(['brave']))
    expect(badges().every((badge) => badge.textContent === 'Brave')).toBe(true)
  }, 20_000)
})

describe('AdminDashboard batch key import provider wiring (plan todo 10 / matrix H1)', () => {
  const SERPAPI_NATIVE_KEY = 'deadbeef'.repeat(8)

  it('parses a native SerpApi key once the batch provider select is serpapi', async () => {
    await mountKeysRoute()
    const overlay = await openBatchOverlay()

    // Given the batch is scoped to a non-Tavily provider
    await act(async () => {
      fireReactChange(batchProviderSelect(overlay), 'serpapi')
    })
    // When a native 64-hex key is pasted
    await act(async () => {
      fireReactChange(batchTextarea(overlay), SERPAPI_NATIVE_KEY)
    })
    // Then the paste yields a parsed row, so the Add action becomes actionable
    await waitFor(() => batchAddButton(overlay).disabled === false)
    expect(batchAddButton(overlay).disabled).toBe(false)
  }, 20_000)

  it('re-parses the same paste to zero rows when the provider switches back to Tavily', async () => {
    await mountKeysRoute()
    const overlay = await openBatchOverlay()

    // Given a native key that parses exactly one row under serpapi
    await act(async () => {
      fireReactChange(batchProviderSelect(overlay), 'serpapi')
    })
    await act(async () => {
      fireReactChange(batchTextarea(overlay), SERPAPI_NATIVE_KEY)
    })
    await waitFor(() => batchAddButton(overlay).disabled === false)

    // When the provider switches back to Tavily with the pasted text unchanged
    await act(async () => {
      fireReactChange(batchProviderSelect(overlay), 'tavily')
    })
    // Then the parse memo recomputes under the Tavily pattern and drops to zero rows
    await waitFor(() => batchAddButton(overlay).disabled === true)
    expect(batchAddButton(overlay).disabled).toBe(true)
  }, 20_000)

  it('leaves Add disabled when a native-provider line is too short to be a key', async () => {
    await mountKeysRoute()
    const overlay = await openBatchOverlay()

    // Given the batch is scoped to a non-Tavily provider
    await act(async () => {
      fireReactChange(batchProviderSelect(overlay), 'serpapi')
    })
    // When the pasted line is shorter than the native-key minimum token length
    await act(async () => {
      fireReactChange(batchTextarea(overlay), 'short')
    })
    await flushUi(150)
    // Then nothing parses and the Add action stays disabled
    expect(batchAddButton(overlay).disabled).toBe(true)
  }, 20_000)

  it('hands a parsed native SerpApi key off to validation instead of early-returning', async () => {
    await mountKeysRoute()
    const overlay = await openBatchOverlay()

    // Given a native key that parses one row under the selected provider
    await act(async () => {
      fireReactChange(batchProviderSelect(overlay), 'serpapi')
    })
    await act(async () => {
      fireReactChange(batchTextarea(overlay), SERPAPI_NATIVE_KEY)
    })
    await waitFor(() => batchAddButton(overlay).disabled === false)

    // When the enabled Add action runs
    await act(async () => {
      fireReactClick(batchAddButton(overlay))
    })

    // Then handleAddKey re-parses under the selected provider rather than taking
    // its zero-row early return: it stages the rows and collapses the overlay
    // into the validation-dialog handoff. `is-closing` is set only by
    // beginKeysBatchClose, which sits after that early return.
    let sawHandoff = false
    await waitFor(() => {
      const current = batchOverlay()
      if (current === null) return sawHandoff
      if (current.classList.contains('is-closing')) sawHandoff = true
      return sawHandoff
    })
    expect(sawHandoff).toBe(true)
  }, 20_000)
})

describe('AdminDashboard key provider facet counts (plan todo 10 / matrix M12)', () => {
  it('renders the /api/keys facets.providers counts inside the provider filter options', async () => {
    await mountKeysRoute({
      providers: [
        { value: 'tavily', count: 2038 },
        { value: 'serpapi', count: 413 },
      ],
    })
    await waitFor(() => providerFilterSelect() !== null)

    const select = providerFilterSelect()
    if (select === null) {
      throw new Error('The provider filter select never mounted')
    }

    // Faceted providers carry their key count ...
    expect(providerOptionText(select, 'serpapi')).toBe('SerpApi (413)')
    expect(providerOptionText(select, 'tavily')).toBe('Tavily (2,038)')
    // ... while providers absent from the facet keep the bare label.
    expect(providerOptionText(select, 'exa')).toBe('Exa')
    expect(providerOptionText(select, 'brave')).toBe('Brave')
    expect(providerOptionText(select, 'all')).toBe('All providers')
  }, 20_000)

  it('keeps bare provider labels when the response carries no provider facet', async () => {
    await mountKeysRoute()
    await waitFor(() => providerFilterSelect() !== null)

    const select = providerFilterSelect()
    if (select === null) {
      throw new Error('The provider filter select never mounted')
    }

    expect(providerOptionText(select, 'serpapi')).toBe('SerpApi')
    expect(providerOptionText(select, 'tavily')).toBe('Tavily')
  }, 20_000)
})