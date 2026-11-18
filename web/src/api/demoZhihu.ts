/**
 * Browser demo fixtures for the Zhihu REST facade + local MCP facade
 * (keenable-integration plan todo 15; route wiring lands in todo 18).
 *
 * Ground truth (committed, READ-ONLY): `src/bin/mock_zhihu.rs` for the native
 * `{Code,Message,Data}` envelopes, the daily quota rows, and the scripted
 * `Code` failures; `src/server/provider_mcp_zhihu.rs` for the local tool
 * catalog (`zhihu_search`, `zhihu_global_search`). Everything here is plain
 * data answered locally — demo mode never calls a network.
 */
import type { DemoMcpToolDefinition } from './demoProviders'

/** Zhihu always answers HTTP 200 with this envelope; `Code` carries the result. */
export interface DemoZhihuEnvelope<T> {
  Code: number
  Message: string
  Data: T
}

export interface DemoZhihuSearchItem {
  Title: string
  Headline: string
  ContentType: string
  ContentID: string
  AuthorSignature: string
  ContentText: string
  Url: string
  CommentCount: number
  VoteUpCount: number
  AuthorName: string
  AuthorAvatar: string
  AuthorBadge: string
  AuthorBadgeText: string
  EditTime: number
  CreateTime: number
  AuthorityLevel: string
  RankingScore: number
}

export interface DemoZhihuSearchData {
  HasMore: boolean
  SearchHashId: string
  Items: DemoZhihuSearchItem[]
}

export interface DemoZhihuQuotaEntry {
  APIID: 'zhihu_search' | 'global_search'
  APIName: string
  TotalQuota: number
  TotalUsed: number
  RemainingQuota: number
}

export type DemoZhihuPlan = 'standard' | 'tight' | 'exhausted'

export type DemoZhihuErrorCode = 10001 | 20001 | 30001 | 90001

function demoZhihuSearchItem(query: string, endpoint: string): DemoZhihuSearchItem {
  return {
    Title: `Mock Zhihu result for ${query}`,
    Headline: `mock ${endpoint} headline`,
    ContentType: 'Article',
    ContentID: '1234567890',
    AuthorSignature: 'mock-author-signature',
    ContentText: `mock content for ${query}`,
    Url: 'https://zhuanlan.zhihu.com/p/mock',
    CommentCount: 0,
    VoteUpCount: 0,
    AuthorName: 'Mock Author',
    AuthorAvatar: 'https://example.invalid/avatar.jpg',
    AuthorBadge: '',
    AuthorBadgeText: '',
    EditTime: 1_788_278_526,
    CreateTime: 1_788_278_525,
    AuthorityLevel: '4',
    RankingScore: 1.25,
  }
}

function demoZhihuSearchData(query: string, endpoint: string): DemoZhihuSearchData {
  return {
    HasMore: false,
    SearchHashId: `mock-${endpoint}-hash`,
    Items: [demoZhihuSearchItem(query, endpoint)],
  }
}

/** `GET /api/zhihu/api/v1/content/zhihu_search` success envelope. */
export function demoZhihuSearchFixture(
  query = 'mock query',
): DemoZhihuEnvelope<DemoZhihuSearchData> {
  return { Code: 0, Message: 'success', Data: demoZhihuSearchData(query, 'zhihu_search') }
}

/** `GET /api/zhihu/api/v1/content/global_search` success envelope. */
export function demoZhihuGlobalSearchFixture(
  query = 'mock query',
): DemoZhihuEnvelope<DemoZhihuSearchData> {
  return { Code: 0, Message: 'success', Data: demoZhihuSearchData(query, 'global_search') }
}

function demoZhihuQuotaEntry(
  apiId: DemoZhihuQuotaEntry['APIID'],
  name: string,
  total: number,
  used: number,
): DemoZhihuQuotaEntry {
  return { APIID: apiId, APIName: name, TotalQuota: total, TotalUsed: used, RemainingQuota: total - used }
}

/**
 * `GET /api/zhihu/api/v1/quota` success envelope. `tight` mirrors the 3/10 and
 * 5/20 QA plan; `exhausted` reports a spent natural day.
 */
export function demoZhihuQuotaFixture(
  plan: DemoZhihuPlan = 'standard',
): DemoZhihuEnvelope<DemoZhihuQuotaEntry[]> {
  if (plan === 'tight') {
    return {
      Code: 0,
      Message: 'success',
      Data: [
        demoZhihuQuotaEntry('global_search', '全网搜索', 20, 15),
        demoZhihuQuotaEntry('zhihu_search', '知乎搜索', 10, 7),
      ],
    }
  }
  const spent = plan === 'exhausted'
  return {
    Code: 0,
    Message: 'success',
    Data: [
      demoZhihuQuotaEntry('global_search', '全网搜索', 5000, spent ? 5000 : 1),
      demoZhihuQuotaEntry('zhihu_search', '知乎搜索', 5000, spent ? 5000 : 2),
    ],
  }
}

const DEMO_ZHIHU_ERROR_MESSAGES: Record<DemoZhihuErrorCode, string> = {
  10001: 'Query cannot be empty',
  20001: 'Authorization failed',
  30001: '请求频率超过限制，请稍后重试',
  90001: '额度数据读取失败',
}

/** HTTP-200 error envelope: the facade classifies the result by `Code`. */
export function demoZhihuErrorFixture(
  code: DemoZhihuErrorCode,
): DemoZhihuEnvelope<null> {
  return { Code: code, Message: DEMO_ZHIHU_ERROR_MESSAGES[code], Data: null }
}

/**
 * `/zhihu/mcp` local-facade catalog: exactly the two tool names, parameters,
 * and inputSchema bounds ratified by the keenable-zhihu-providers SPEC.
 */
export const ZHIHU_MCP_TOOLS: DemoMcpToolDefinition[] = [
  {
    name: 'zhihu_search',
    description: 'Zhihu content search — in-site search over Zhihu answers, articles, and posts',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Non-empty search keyword (required).' },
        count: {
          type: 'integer',
          description: 'Number of results to return.',
          minimum: 1,
          maximum: 10,
        },
        sort_by: {
          type: 'string',
          description:
            'Sort expression `field:direction:(min,max)` — fields CommentCount/VoteUpCount/EditTime/CreateTime, direction asc/desc/none, optional inclusive Unix-second bounds.',
        },
      },
      required: ['query'],
      additionalProperties: false,
    },
  },
  {
    name: 'zhihu_global_search',
    description: 'Zhihu global search — web-wide search with realtime/static database selection',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Non-empty search keyword (required).' },
        count: {
          type: 'integer',
          description: 'Number of results to return.',
          minimum: 1,
          maximum: 20,
        },
        search_db: {
          type: 'string',
          description: 'Search database to query.',
          enum: ['all', 'realtime', 'static'],
          default: 'all',
        },
        filter: {
          type: 'string',
          description:
            'Advanced filter grammar: host ==/!= quoted strings, publish_time ==/!=/>/>=/</<= Unix-second values, uppercase AND/OR with parentheses.',
        },
      },
      required: ['query'],
      additionalProperties: false,
    },
  },
]
