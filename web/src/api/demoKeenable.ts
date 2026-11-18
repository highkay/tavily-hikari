/**
 * Browser demo fixtures for the Keenable REST facade + hosted MCP passthrough
 * (keenable-integration plan todo 15; route wiring lands in todo 18).
 *
 * Ground truth (committed, READ-ONLY): `src/bin/mock_keenable.rs` for the
 * native success bodies, the passive credit harvest fields
 * (`usage.credits_remaining`, `X-RateLimit-*`), and the scripted failures;
 * `src/server/provider_mcp_keenable.rs` for the hosted tool names. Everything
 * here is plain data answered locally — demo mode never calls a network.
 */
import type { DemoMcpToolDefinition } from './demoProviders'

/** Native `usage` object charged on every billable Keenable call. */
export interface DemoKeenableUsage {
  sku: 'search' | 'fetch'
  amount: number
  credits: number
  paid: boolean
  credits_remaining: number
}

export interface DemoKeenableSearchResult {
  title: string
  url: string
  description: string
  snippet: string
  published_at: string
}

/** `POST /api/keenable/v1/search` success body. */
export interface DemoKeenableSearchFixture {
  query: string
  mode: string
  results: DemoKeenableSearchResult[]
  usage: DemoKeenableUsage
}

/** `GET /api/keenable/v1/fetch?url=...` success body. */
export interface DemoKeenableFetchFixture {
  url: string
  title: string
  content: string
  description: string
  truncated: boolean
  total_chars: number
  usage: DemoKeenableUsage
}

/** Closed `type`/`code` error envelope shared by both REST surfaces. */
export interface DemoKeenableErrorBody {
  error: string
  type: string
  code: string
  message: string
  request_id: string
  retryable: boolean
  param?: string
  retry_after_seconds?: number
  retryAfter?: number
  billing_url?: string
  auto_top_up_url?: string
}

/** Starting balance of the mock upstream after one billable call. */
const DEMO_CREDITS_REMAINING = 99_999

function demoKeenableUsage(sku: 'search' | 'fetch'): DemoKeenableUsage {
  return {
    sku,
    amount: 1,
    credits: 1,
    paid: false,
    credits_remaining: DEMO_CREDITS_REMAINING,
  }
}

export function demoKeenableSearchFixture(
  query = 'mock query',
  mode = 'pro',
): DemoKeenableSearchFixture {
  return {
    query,
    mode,
    results: [
      {
        title: 'Mock Keenable Result',
        url: 'https://example.com/1',
        description: 'mock description',
        snippet: 'mock snippet',
        published_at: '2026-01-01',
      },
    ],
    usage: demoKeenableUsage('search'),
  }
}

export function demoKeenableFetchFixture(url = 'https://example.com/1'): DemoKeenableFetchFixture {
  const content = 'mock page content'
  return {
    url,
    title: 'Mock Page',
    content,
    description: '',
    truncated: false,
    total_chars: content.length,
    usage: demoKeenableUsage('fetch'),
  }
}

/** Passive-harvest headers carried by the mock's keyed success responses. */
export function demoKeenableRateLimitHeaders(
  creditsRemaining = DEMO_CREDITS_REMAINING,
): Record<string, string> {
  return {
    'x-ratelimit-limit': '10',
    'x-ratelimit-remaining': String(creditsRemaining),
    'x-ratelimit-reset': '1',
    'x-request-id': 'mock-request-id',
  }
}

/** 401: the zero-cost probe and every keyed call classify this as auth-fail. */
export function demoKeenableInvalidKeyFixture(): DemoKeenableErrorBody {
  return {
    error: 'Authentication failed',
    type: 'authentication_error',
    code: 'invalid_api_key',
    message: 'Invalid API key',
    request_id: 'mock-request-id',
    retryable: false,
  }
}

/** 402: credits spent; the key is quarantined as exhausted. */
export function demoKeenableInsufficientCreditsFixture(): DemoKeenableErrorBody {
  return {
    error: 'Insufficient credits',
    type: 'billing_error',
    code: 'insufficient_credits',
    message: "This month's free requests and purchased credits are used up.",
    request_id: 'mock-request-id',
    retryable: false,
    billing_url: 'https://example.invalid/billing',
    auto_top_up_url: 'https://example.invalid/auto-top-up',
  }
}

/** 429: rate limited with the vendor's `retry_after_seconds` hint. */
export function demoKeenableRateLimitedFixture(): DemoKeenableErrorBody {
  return {
    error: 'Rate limit exceeded',
    type: 'rate_limit_error',
    code: 'rate_limit_per_second',
    message: 'Too many requests; your organization is limited to 10 requests per second.',
    request_id: 'mock-request-id',
    retryable: true,
    retry_after_seconds: 1,
    retryAfter: 1,
  }
}

/** 400: the missing-query arm the zero-cost validation probe relies on. */
export function demoKeenableMissingParameterFixture(
  param = 'query',
): DemoKeenableErrorBody {
  return {
    error: 'Missing required parameter',
    type: 'invalid_request_error',
    code: 'missing_parameter',
    message: `"${param}" is required`,
    request_id: 'mock-request-id',
    retryable: false,
    param,
  }
}

/**
 * `/keenable/mcp` tools/list catalog: Keenable's hosted passthrough exposes
 * exactly these two vendor tools (the mock forwards the vendor catalog).
 */
export const KEENABLE_MCP_TOOLS: DemoMcpToolDefinition[] = [
  {
    name: 'search_web_pages',
    description: 'Search the live web with Keenable.',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'search_web_pages query' },
      },
      required: ['query'],
    },
  },
  {
    name: 'fetch_page_content',
    description: 'Read any page as clean markdown.',
    inputSchema: {
      type: 'object',
      properties: {
        url: { type: 'string', description: 'fetch_page_content url' },
      },
      required: ['url'],
    },
  },
]
