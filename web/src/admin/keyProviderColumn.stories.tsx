import { useState } from 'react'
import type { Meta, StoryObj } from '@storybook/react-vite'

import type { ProviderId } from '../api'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useTranslate } from '../i18n'
import {
  KeyProviderBadge,
  KeyProviderFilter,
  KeyProviderSelect,
  PROVIDER_FILTER_ALL,
} from './keyProviderColumn'

/**
 * Plan todo 22 visual proof: a mixed-provider admin key table fragment with the
 * text provider badges (legacy NULL provider renders Tavily) and the provider
 * filter row control. Used as the happy-path QA capture target.
 */
function KeyProviderColumnProof({
  initialAddProvider = 'keenable',
}: {
  initialAddProvider?: ProviderId
}): JSX.Element {
  const shared = useTranslate().admin.providers.shared
  const [addProvider, setAddProvider] = useState<ProviderId>(initialAddProvider)
  const rows: Array<{ id: string; provider?: unknown }> = [
    { id: 'legacy-key', provider: undefined },
    { id: 'serpapi-key', provider: 'serpapi' },
    { id: 'exa-key', provider: 'exa' },
    { id: 'brave-key', provider: 'brave' },
    { id: 'keenable-key', provider: 'keenable' },
    { id: 'zhihu-key', provider: 'zhihu' },
  ]
  return (
    <div className="surface panel" style={{ padding: 24, display: 'grid', gap: 16 }}>
      <div>
        <h2>Admin key provider column</h2>
        <p className="panel-description">
          Text badges only, one provider per row; legacy rows without a provider render Tavily.
        </p>
      </div>
      <KeyProviderFilter value={PROVIDER_FILTER_ALL} onChange={() => undefined} />
      <KeyProviderSelect value={addProvider} onChange={setAddProvider} />
      <Table className="jobs-table api-keys-table">
        <TableHeader>
          <TableRow>
            <TableHead>Key</TableHead>
            <TableHead>{shared.tableHeader}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row) => (
            <TableRow key={row.id}>
              <TableCell>
                <code>{row.id}</code>
              </TableCell>
              <TableCell>
                <KeyProviderBadge provider={row.provider} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

const meta = {
  title: 'Admin/Fragments/KeyProviderColumn',
  component: KeyProviderColumnProof,
  tags: ['autodocs'],
  parameters: { layout: 'fullscreen' },
} satisfies Meta<typeof KeyProviderColumnProof>

export default meta

type Story = StoryObj<typeof meta>

export const MixedProviderTable: Story = {
  globals: { language: 'en', themeMode: 'light' },
  parameters: { viewport: { defaultViewport: '1440-device-desktop' } },
  play: ({ canvasElement }) => {
    const text = canvasElement.ownerDocument.body.textContent ?? ''
    const filter = canvasElement.querySelector<HTMLSelectElement>('select[data-provider-filter]')
    const filterValues = Array.from(filter?.options ?? [], (option) => option.value)
    for (const provider of ['keenable', 'zhihu']) {
      if (!filterValues.includes(provider)) {
        throw new Error(`Expected keys filter option: ${provider}`)
      }
    }
    for (const expected of [
      'Provider',
      'Tavily',
      'SerpApi',
      'Exa',
      'Brave',
      'Keenable',
      'Zhihu',
      'All providers',
      'X-API-Key',
    ]) {
      if (!text.includes(expected)) {
        throw new Error(`Expected provider column proof to contain: ${expected}`)
      }
    }
  },
}

export const MixedProviderTableZh: Story = {
  ...MixedProviderTable,
  globals: { language: 'zh', themeMode: 'light' },
}

export const MixedProviderTableDark: Story = {
  ...MixedProviderTable,
  globals: { language: 'en', themeMode: 'dark' },
}

export const MixedProviderTableMobile: Story = {
  ...MixedProviderTable,
  parameters: { viewport: { defaultViewport: '0390-device-short' } },
}

export const ZhihuProviderIntake: Story = {
  ...MixedProviderTable,
  args: { initialAddProvider: 'zhihu' },
  play: ({ canvasElement }) => {
    const hint = canvasElement.querySelector('[data-native-slot="zhihu"]')?.textContent ?? ''
    if (!hint.includes('Access Secret') || !hint.includes('Authorization: Bearer')) {
      throw new Error('Expected the Zhihu native credential slot hint')
    }
  },
}

export const ZhihuProviderIntakeDarkMobile: Story = {
  ...ZhihuProviderIntake,
  globals: { language: 'zh', themeMode: 'dark' },
  parameters: { viewport: { defaultViewport: '0390-device-short' } },
}