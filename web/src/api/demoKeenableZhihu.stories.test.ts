import '../../test/happydom'

import { describe, expect, it } from 'bun:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'

import meta, * as stories from './demoKeenableZhihu.stories'

describe('Keenable and Zhihu demo fixture Storybook proofs', () => {
  it('publishes desktop, mobile, light, and dark gallery variants', () => {
    expect(meta).toMatchObject({ title: 'Demo/KeenableZhihuFixtures' })
    expect(stories.LocalProviderFixtures.globals).toMatchObject({ themeMode: 'light' })
    expect(stories.LocalProviderFixturesDark.globals).toMatchObject({ themeMode: 'dark' })
    expect(stories.LocalProviderFixturesMobile.parameters).toMatchObject({
      viewport: { defaultViewport: 'mobile1' },
    })
    expect(stories.LocalProviderFixturesDarkMobile.globals).toMatchObject({ themeMode: 'dark' })
  })

  it('renders every fixture card and executes the local MCP story play test', async () => {
    document.body.innerHTML = renderToStaticMarkup(createElement(meta.component))
    for (const id of [
      'keenable-search',
      'keenable-401',
      'keenable-402',
      'keenable-429',
      'zhihu-search',
      'zhihu-quota',
      'mcp-catalogs',
    ]) {
      expect(document.body.querySelector(`[data-fixture="${id}"]`)).not.toBeNull()
    }
    expect(document.body.textContent).toContain('search_web_pages')
    expect(document.body.textContent).toContain('zhihu_global_search')

    const play = stories.LocalProviderFixtures.play
    if (!play) throw new Error('Expected the demo fixture story play test')
    await play({ canvasElement: document.body } as Parameters<typeof play>[0])
  })
})
