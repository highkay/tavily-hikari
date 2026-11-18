import { describe, expect, it } from 'bun:test'

import {
  mergeRequestKindCatalog,
  uniqueSelectedRequestKinds,
  type TokenLogRequestKindOption,
} from './tokenLogRequestKinds'

/** The eight kinds added by keenable-integration todo 14. */
const NEW_PROVIDER_KINDS = [
  ['api:keenable/search', 'API | keenable/search', 'api', 'billable'],
  ['api:keenable/fetch', 'API | keenable/fetch', 'api', 'billable'],
  ['mcp:keenable/search_web_pages', 'MCP | keenable/search_web_pages', 'mcp', 'billable'],
  ['mcp:keenable/fetch_page_content', 'MCP | keenable/fetch_page_content', 'mcp', 'billable'],
  ['api:zhihu/zhihu_search', 'API | zhihu/zhihu_search', 'api', 'billable'],
  ['api:zhihu/global_search', 'API | zhihu/global_search', 'api', 'billable'],
  ['mcp:zhihu/zhihu_search', 'MCP | zhihu/zhihu_search', 'mcp', 'billable'],
  ['mcp:zhihu/global_search', 'MCP | zhihu/global_search', 'mcp', 'billable'],
] as const

function catalogByKey(): Map<string, TokenLogRequestKindOption> {
  return new Map(mergeRequestKindCatalog([]).map((option) => [option.key, option]))
}

describe('keenable and zhihu request kind registration (plan todo 15)', () => {
  it('registers all eight kinds with canonical labels and billing groups', () => {
    const byKey = catalogByKey()
    for (const [key, label, protocol, billing] of NEW_PROVIDER_KINDS) {
      expect(byKey.get(key)).toMatchObject({
        key,
        label,
        protocol_group: protocol,
        billing_group: billing,
      })
    }
  })

  it('keeps every new kind selected instead of folding it into an unknown bucket', () => {
    const selected = NEW_PROVIDER_KINDS.map(([key]) => key)
    expect(uniqueSelectedRequestKinds(selected)).toEqual(selected)
  })

  it('keeps the legacy provider kinds registered unchanged', () => {
    const byKey = catalogByKey()
    expect(byKey.get('api:serpapi/search')?.label).toBe('API | serpapi/search')
    expect(byKey.get('mcp:brave/llm_context')?.label).toBe('MCP | brave/llm_context')
  })
})
