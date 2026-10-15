import type { Meta, StoryObj } from '@storybook/react'

import type { AnalysisProvidersSnapshot } from '../api'
import AnalysisProvidersScreen from './AnalysisProvidersScreen'

/**
 * Storybook proof for 分析 > 提供方 (multi-provider-alignment-completion C1):
 * one snapshot carries all three windows, so the tab strip is client-side state
 * and every window renders from the same fixture without a refetch.
 */

const snapshot: AnalysisProvidersSnapshot = {
  generatedAt: 1_776_041_600,
  windows: {
    last24h: [
      { provider: 'tavily', requests: 1607, success: 1500, errors: 100, quotaExhausted: 7, credits: 1498 },
      { provider: 'serpapi', requests: 413, success: 398, errors: 12, quotaExhausted: 3, credits: 396 },
      { provider: 'exa', requests: 128, success: 121, errors: 7, quotaExhausted: 0, credits: 120 },
      { provider: 'brave', requests: 96, success: 88, errors: 6, quotaExhausted: 2, credits: 87 },
    ],
    last7d: [
      { provider: 'tavily', requests: 9842, success: 9310, errors: 486, quotaExhausted: 46, credits: 9288 },
      { provider: 'serpapi', requests: 2871, success: 2790, errors: 62, quotaExhausted: 19, credits: 2784 },
      { provider: 'brave', requests: 604, success: 573, errors: 27, quotaExhausted: 4, credits: 570 },
      { provider: 'exa', requests: 517, success: 498, errors: 19, quotaExhausted: 0, credits: 495 },
    ],
    last30d: [
      { provider: 'tavily', requests: 41_208, success: 39_114, errors: 1_902, quotaExhausted: 192, credits: 39_040 },
      { provider: 'serpapi', requests: 11_463, success: 11_102, errors: 288, quotaExhausted: 73, credits: 11_071 },
      { provider: 'exa', requests: 2_204, success: 2_118, errors: 86, quotaExhausted: 0, credits: 2_110 },
      { provider: 'brave', requests: 1_980, success: 1_871, errors: 96, quotaExhausted: 13, credits: 1_862 },
    ],
  },
}

const meta = {
  title: 'Admin/Components/AnalysisProvidersScreen',
  component: AnalysisProvidersScreen,
  args: {
    snapshot,
    loading: false,
    error: null,
    language: 'en',
    onRetry: () => undefined,
  },
  parameters: {
    docs: {
      description: {
        component:
          'Per-provider request/success/error/quota-exhausted/credit totals for the selected analysis window. Data: GET /api/analysis/providers (one snapshot, three windows, client-side tab switch).',
      },
    },
  },
} satisfies Meta<typeof AnalysisProvidersScreen>

export default meta

type Story = StoryObj<typeof meta>

export const Default: Story = {
  parameters: {
    docs: {
      description: { story: 'Last-24-hour window with all four providers present.' },
    },
  },
  play: async ({ canvasElement }) => {
    const rows = canvasElement.ownerDocument.querySelectorAll('tbody tr[data-provider]')
    if (rows.length !== 4) {
      throw new Error(`Expected four provider rows, found ${rows.length}`)
    }
  },
}

export const Last30Days: Story = {
  args: { snapshot },
  parameters: {
    docs: {
      description: {
        story:
          'The 30-day window comes from the same snapshot: switching tabs is client-side and never refetches.',
      },
    },
  },
}

export const Chinese: Story = {
  args: { language: 'zh' },
  parameters: {
    docs: {
      description: { story: '中文目录：提供方 / 请求数 / 成功 / 错误 / 配额耗尽 / 积分。' },
    },
  },
  play: async ({ canvasElement }) => {
    const text = canvasElement.ownerDocument.body.textContent ?? ''
    if (!text.includes('最近24小时') || !text.includes('配额耗尽')) {
      throw new Error('Expected the Chinese catalog labels in the provider analysis story')
    }
  },
}

export const EmptyWindow: Story = {
  args: {
    snapshot: { generatedAt: snapshot.generatedAt, windows: { last24h: [], last7d: [], last30d: [] } },
  },
  parameters: {
    docs: {
      description: { story: 'A snapshot with no provider activity in any window renders the empty state.' },
    },
  },
  play: async ({ canvasElement }) => {
    const text = canvasElement.ownerDocument.body.textContent ?? ''
    if (!text.includes('No provider activity in this window.')) {
      throw new Error('Expected the empty-window copy in the provider analysis story')
    }
  },
}

export const NoSnapshot: Story = {
  args: { snapshot: null },
  parameters: {
    docs: {
      description: { story: 'Before the first snapshot resolves the view keeps its empty placeholder.' },
    },
  },
}

export const Loading: Story = {
  args: { snapshot: null, loading: true },
  parameters: {
    docs: { description: { story: 'Initial load renders the shared admin loading region.' } },
  },
}

export const ErrorState: Story = {
  args: { snapshot: null, error: 'upstream unavailable' },
  parameters: {
    docs: {
      description: { story: 'A failed load surfaces the message with a retry affordance.' },
    },
  },
  play: async ({ canvasElement }) => {
    const retry = canvasElement.ownerDocument.querySelector('button.btn-outline')
    if (retry === null) {
      throw new Error('Expected a retry button in the provider analysis error story')
    }
  },
}

export const StaleAfterRefreshError: Story = {
  args: { snapshot, error: 'refresh failed, showing the last snapshot' },
  parameters: {
    docs: {
      description: {
        story: 'A refresh failure keeps the last good snapshot on screen and offers retry inline.',
      },
    },
  },
}
