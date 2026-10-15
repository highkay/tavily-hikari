import { useCallback, useEffect, useMemo, useState } from 'react'

import { fetchAdminAnalysisProviders, type AnalysisProvidersSnapshot } from '../api'
import type { AdminPathRoute } from './routes'

/**
 * Data lane of the 分析 > 提供方 sub-view, extracted from
 * `AdminDashboardRuntime.tsx` so the legacy runtime keeps only the wiring lines
 * (multi-provider-alignment-completion C1).
 *
 * Mirrors the runtime's `loadAnalysisPressure` semantics: one snapshot per
 * activation, abort-on-unmount, abort never writes state, a failure surfaces as
 * a retryable error while the last good snapshot stays on screen.
 */
export interface AnalysisProvidersState {
  snapshot: AnalysisProvidersSnapshot | null
  loading: boolean
  error: string | null
  /** Clears the error and reloads on a fresh controller (retry button). */
  retry: () => void
  /** Reloads on a caller-owned signal (manual refresh task list). */
  reload: (signal?: AbortSignal) => Promise<void>
}

function isAnalysisProvidersRoute(route: AdminPathRoute): boolean {
  return route.name === 'module' && route.module === 'analysis' && route.analysisView === 'providers'
}

export function useAnalysisProviders(route: AdminPathRoute): AnalysisProvidersState {
  const active = isAnalysisProvidersRoute(route)
  const [snapshot, setSnapshot] = useState<AnalysisProvidersSnapshot | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const reload = useCallback(async (signal?: AbortSignal) => {
    setLoading(true)
    try {
      const next = await fetchAdminAnalysisProviders(signal)
      if (signal?.aborted) return
      setSnapshot(next)
      setError(null)
    } catch (err) {
      if (signal?.aborted || (err as Error).name === 'AbortError') return
      setError(err instanceof Error ? err.message : 'Unexpected error occurred')
    } finally {
      if (!signal?.aborted) {
        setLoading(false)
      }
    }
  }, [])

  useEffect(() => {
    if (!active) return
    const controller = new AbortController()
    void reload(controller.signal)
    return () => controller.abort()
  }, [active, reload, route])

  const retry = useCallback(() => {
    setError(null)
    const controller = new AbortController()
    void reload(controller.signal)
  }, [reload])

  return useMemo(
    () => ({ snapshot, loading, error, retry, reload }),
    [snapshot, loading, error, retry, reload],
  )
}
