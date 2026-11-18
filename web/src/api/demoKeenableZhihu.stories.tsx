import type { Meta, StoryObj } from '@storybook/react-vite'

import { Badge } from '../components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import {
  KEENABLE_MCP_TOOLS,
  demoKeenableInsufficientCreditsFixture,
  demoKeenableInvalidKeyFixture,
  demoKeenableRateLimitedFixture,
  demoKeenableSearchFixture,
} from './demoKeenable'
import { handleProviderDemoRoute } from './demoProviders'
import {
  ZHIHU_MCP_TOOLS,
  demoZhihuQuotaFixture,
  demoZhihuSearchFixture,
} from './demoZhihu'

interface FixtureCardProps {
  id: string
  title: string
  path: string
  status: number
  payload: unknown
  detail?: string
}

function FixtureCard({ id, title, path, status, payload, detail }: FixtureCardProps): JSX.Element {
  return (
    <Card data-fixture={id} className="min-w-0">
      <CardHeader className="gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <CardTitle>{title}</CardTitle>
          <Badge variant={status === 200 ? 'success' : status === 429 ? 'warning' : 'destructive'}>
            HTTP {status}{detail ? ` · ${detail}` : ''}
          </Badge>
        </div>
        <CardDescription className="break-all font-mono text-xs">{path}</CardDescription>
      </CardHeader>
      <CardContent>
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-[24px] bg-muted/45 p-4 font-mono text-xs leading-relaxed text-foreground shadow-clayPressed">
          <code>{JSON.stringify(payload, null, 2)}</code>
        </pre>
      </CardContent>
    </Card>
  )
}

function DemoProviderFixtureGallery(): JSX.Element {
  return (
    <main className="app-shell min-h-screen px-4 py-6 text-foreground md:px-8 md:py-8">
      <div className="mx-auto flex max-w-7xl flex-col gap-6">
        <section className="surface app-header flex flex-col gap-4 p-6 md:p-8">
          <Badge variant="info" className="w-fit">Local demo fixtures</Badge>
          <div className="flex flex-col gap-3">
            <h1 className="font-display text-3xl font-black leading-tight md:text-5xl">Keenable and Zhihu, without upstream traffic</h1>
            <p className="max-w-3xl text-sm font-medium leading-relaxed text-muted-foreground md:text-base">
              Native response envelopes, scripted failures, quota snapshots, and MCP catalogs are answered inside the browser demo runtime.
            </p>
          </div>
        </section>
        <section className="grid items-start gap-6 xl:grid-cols-2">
          <FixtureCard
            id="keenable-search"
            title="Keenable search success"
            path="POST /api/keenable/v1/search"
            status={200}
            payload={demoKeenableSearchFixture('demo mode', 'pro')}
          />
          <FixtureCard
            id="zhihu-search"
            title="Zhihu native envelope"
            path="GET /api/zhihu/api/v1/content/zhihu_search"
            status={200}
            detail="Code 0"
            payload={demoZhihuSearchFixture('demo mode')}
          />
          <FixtureCard
            id="keenable-401"
            title="Keenable invalid key"
            path="X-API-Key: key-auth"
            status={401}
            payload={demoKeenableInvalidKeyFixture()}
          />
          <FixtureCard
            id="keenable-402"
            title="Keenable credits exhausted"
            path="X-API-Key: key-exhausted"
            status={402}
            payload={demoKeenableInsufficientCreditsFixture()}
          />
          <FixtureCard
            id="keenable-429"
            title="Keenable rate limited"
            path="X-API-Key: key-rate"
            status={429}
            payload={demoKeenableRateLimitedFixture()}
          />
          <FixtureCard
            id="zhihu-quota"
            title="Zhihu tight daily quota"
            path="Authorization: Bearer tok-quota-tight"
            status={200}
            detail="Code 0"
            payload={demoZhihuQuotaFixture('tight')}
          />
          <div className="xl:col-span-2">
            <FixtureCard
              id="mcp-catalogs"
              title="MCP tool catalogs"
              path="POST /keenable/mcp · POST /zhihu/mcp"
              status={200}
              payload={{
                keenable: KEENABLE_MCP_TOOLS.map((tool) => tool.name),
                zhihu: ZHIHU_MCP_TOOLS.map((tool) => tool.name),
              }}
            />
          </div>
        </section>
      </div>
    </main>
  )
}

const meta = {
  title: 'Demo/KeenableZhihuFixtures',
  component: DemoProviderFixtureGallery,
  tags: ['autodocs'],
  parameters: { layout: 'fullscreen' },
} satisfies Meta<typeof DemoProviderFixtureGallery>

export default meta

type Story = StoryObj<typeof meta>

export const LocalProviderFixtures: Story = {
  globals: { language: 'en', themeMode: 'light' },
  parameters: { viewport: { defaultViewport: '1440-device-desktop' } },
  play: async ({ canvasElement }) => {
    for (const id of ['keenable-search', 'keenable-401', 'keenable-402', 'keenable-429', 'zhihu-search', 'zhihu-quota', 'mcp-catalogs']) {
      if (!canvasElement.querySelector(`[data-fixture="${id}"]`)) {
        throw new Error(`Expected demo fixture card: ${id}`)
      }
    }
    const response = await handleProviderDemoRoute('/keenable/mcp', 'POST', {
      body: JSON.stringify({ jsonrpc: '2.0', id: 18, method: 'tools/list' }),
    })
    const body = await response?.json() as { result?: { tools?: Array<{ name: string }> } } | undefined
    const names = body?.result?.tools?.map((tool) => tool.name) ?? []
    if (names.join(',') !== 'search_web_pages,fetch_page_content') {
      throw new Error('Expected the local Keenable MCP catalog in the story test')
    }
  },
}

export const LocalProviderFixturesDark: Story = {
  ...LocalProviderFixtures,
  globals: { language: 'en', themeMode: 'dark' },
}

export const LocalProviderFixturesMobile: Story = {
  ...LocalProviderFixtures,
  parameters: { viewport: { defaultViewport: 'mobile1' } },
}

export const LocalProviderFixturesDarkMobile: Story = {
  ...LocalProviderFixtures,
  globals: { language: 'zh', themeMode: 'dark' },
  parameters: { viewport: { defaultViewport: 'mobile1' } },
}
