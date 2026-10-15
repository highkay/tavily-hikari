import { LEGACY_PROVIDER_ID } from './providers'
import { requestJson } from './runtime'

/**
 * `GET /api/analysis/providers` contract (multi-provider-alignment-completion C1).
 *
 * One snapshot carries all three analysis windows — the same shape the rankings
 * endpoint uses — so the 24h/7d/30d tab switch is client-side and never
 * re-fetches. Legacy `NULL` provider rows are already merged into `tavily`
 * server-side; {@link parseAnalysisProviderRow} repeats that coalesce for absent
 * fields and keeps an unknown non-empty wire id verbatim instead of mislabeling
 * it as Tavily (same rule as `providerLabel`).
 *
 * The wire shape is verified against the emitter (`src/server/dto.rs`
 * `AnalysisProviderCountView` / `AnalysisProvidersView`, `Serialize` with no
 * rename), so the parse reads exactly those keys: a renamed field should fail
 * loudly in tests rather than be silently tolerated here. Absent or non-numeric
 * counters still read 0 and an absent window reads empty, because a boundary
 * parse never throws on a malformed payload.
 */
export type AnalysisProviderWindowKey = 'last24h' | 'last7d' | 'last30d'

/** Stable tab order of the provider analysis view. */
export const ANALYSIS_PROVIDER_WINDOW_KEYS: readonly AnalysisProviderWindowKey[] = [
  'last24h',
  'last7d',
  'last30d',
]

/** Per-provider counters for one window; `credits` are local-estimated business credits. */
export interface AnalysisProviderRow {
  provider: string
  requests: number
  success: number
  errors: number
  quotaExhausted: number
  credits: number
}

export interface AnalysisProvidersSnapshot {
  generatedAt: number
  windows: Record<AnalysisProviderWindowKey, AnalysisProviderRow[]>
}

type RecordLike = Record<string, unknown>

function isRecordLike(value: unknown): value is RecordLike {
  return typeof value === 'object' && value !== null
}

function readCount(source: RecordLike, key: string): number {
  const candidate = source[key]
  return typeof candidate === 'number' && Number.isFinite(candidate) ? candidate : 0
}

function readProvider(source: RecordLike): string {
  const candidate = source.provider
  return typeof candidate === 'string' && candidate.trim().length > 0
    ? candidate
    : LEGACY_PROVIDER_ID
}

export function parseAnalysisProviderRow(value: unknown): AnalysisProviderRow {
  const source = isRecordLike(value) ? value : {}
  return {
    provider: readProvider(source),
    requests: readCount(source, 'requests'),
    success: readCount(source, 'success'),
    errors: readCount(source, 'errors'),
    quotaExhausted: readCount(source, 'quota_exhausted'),
    credits: readCount(source, 'credits'),
  }
}

function parseWindowRows(value: unknown): AnalysisProviderRow[] {
  return Array.isArray(value) ? value.map(parseAnalysisProviderRow) : []
}

export function parseAnalysisProvidersSnapshot(value: unknown): AnalysisProvidersSnapshot {
  const source = isRecordLike(value) ? value : {}
  const windows = isRecordLike(source.windows) ? source.windows : {}
  return {
    generatedAt: readCount(source, 'generated_at'),
    windows: {
      last24h: parseWindowRows(windows.last24h),
      last7d: parseWindowRows(windows.last7d),
      last30d: parseWindowRows(windows.last30d),
    },
  }
}

export function fetchAdminAnalysisProviders(
  signal?: AbortSignal,
): Promise<AnalysisProvidersSnapshot> {
  return requestJson<unknown>('/api/analysis/providers', { signal }).then(
    parseAnalysisProvidersSnapshot,
  )
}
