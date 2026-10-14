import { describe, expect, it } from 'bun:test'

import {
  extractApiKeyImportEntriesFromText,
  extractApiKeyImportEntryFromLine,
  extractRegistrationIpFromLine,
  extractTvlyDevApiKeyFromLine,
  extractTvlyDevApiKeysFromText,
} from './api-key-extract'

describe('api key import parser', () => {
  it('extracts the first key and first public ipv4 from a mixed line', () => {
    expect(extractApiKeyImportEntryFromLine('mail@example.com | tvly-dev-abc | 8.8.8.8')).toEqual({
      api_key: 'tvly-dev-abc',
      registration_ip: '8.8.8.8',
    })
  })

  it('extracts public ipv6 tokens and ignores delimiters', () => {
    expect(extractApiKeyImportEntryFromLine('note;2606:4700:4700::1111,tvly-dev-ghi')).toEqual({
      api_key: 'tvly-dev-ghi',
      registration_ip: '2606:4700:4700::1111',
    })
  })

  it('ignores private and reserved addresses', () => {
    expect(extractRegistrationIpFromLine('user | tvly-dev-private | 10.0.0.1 | 192.168.1.2')).toBeNull()
    expect(extractRegistrationIpFromLine('tvly-dev-doc | 2001:db8::1')).toBeNull()
  })

  it('returns one structured entry per matched line', () => {
    expect(
      extractApiKeyImportEntriesFromText([
        'a tvly-dev-first 1.1.1.1',
        'skip me',
        'b;tvly-dev-second;8.8.4.4',
      ].join('\n')),
    ).toEqual([
      { api_key: 'tvly-dev-first', registration_ip: '1.1.1.1' },
      { api_key: 'tvly-dev-second', registration_ip: '8.8.4.4' },
    ])
  })
})

describe('provider-aware api key import', () => {
  // Dummies only, never a live provider key. `deadbeef` x8 is the 64-hex sample
  // the release QA pastes into the add-key dialog and then cancels.
  const SERPAPI_KEY = 'deadbeef'.repeat(8)
  const EXA_KEY = '123e4567-e89b-12d3-a456-426614174000'
  const BRAVE_KEY = 'BSAdummy0123456789abcdef'

  it('extracts a serpapi 64-hex key line', () => {
    expect(extractApiKeyImportEntriesFromText(SERPAPI_KEY, 'serpapi')).toEqual([
      { api_key: SERPAPI_KEY, registration_ip: null },
    ])
  })

  it('extracts an exa UUID key line', () => {
    expect(extractApiKeyImportEntriesFromText(EXA_KEY, 'exa')).toEqual([
      { api_key: EXA_KEY, registration_ip: null },
    ])
  })

  it('extracts a brave BSA-prefixed key line', () => {
    expect(extractApiKeyImportEntriesFromText(BRAVE_KEY, 'brave')).toEqual([
      { api_key: BRAVE_KEY, registration_ip: null },
    ])
  })

  it('pairs the key with the registration ip on a delimited line', () => {
    expect(extractApiKeyImportEntriesFromText('serpdummy0123456789,1.2.3.4', 'serpapi')).toEqual([
      { api_key: 'serpdummy0123456789', registration_ip: '1.2.3.4' },
    ])
  })

  it('skips a leading registration ip when looking for the key', () => {
    expect(extractApiKeyImportEntriesFromText('8.8.8.8 serpdummy0123456789', 'serpapi')).toEqual([
      { api_key: 'serpdummy0123456789', registration_ip: '8.8.8.8' },
    ])
  })

  it('extracts one entry per key line and ignores prose lines', () => {
    const text = ['deadbeefdeadbeef', 'paste your key here', 'BSAdummy0123456789,9.9.9.9'].join(
      '\n',
    )
    expect(extractApiKeyImportEntriesFromText(text, 'brave')).toEqual([
      { api_key: 'deadbeefdeadbeef', registration_ip: null },
      { api_key: 'BSAdummy0123456789', registration_ip: '9.9.9.9' },
    ])
  })

  it('extracts a tvly- production key under tavily', () => {
    expect(extractApiKeyImportEntriesFromText('tvly-prod0123456789', 'tavily')).toEqual([
      { api_key: 'tvly-prod0123456789', registration_ip: null },
    ])
  })

  it('still extracts a legacy tvly-dev- key under tavily', () => {
    expect(extractApiKeyImportEntriesFromText('tvly-dev-legacy01 1.1.1.1', 'tavily')).toEqual([
      { api_key: 'tvly-dev-legacy01', registration_ip: '1.1.1.1' },
    ])
  })

  it('yields nothing for empty input', () => {
    expect(extractApiKeyImportEntriesFromText('', 'serpapi')).toEqual([])
    expect(extractApiKeyImportEntriesFromText('\n\n', 'serpapi')).toEqual([])
  })

  it('yields nothing for an ip-only line', () => {
    // 7 chars: rejected by the length floor as well as by the ip exclusion.
    expect(extractApiKeyImportEntriesFromText('8.8.8.8', 'serpapi')).toEqual([])
    // 20 chars: only the "never the registration ip" rule rejects this one.
    expect(extractApiKeyImportEntriesFromText('2606:4700:4700::1111', 'serpapi')).toEqual([])
  })

  it('yields nothing for a token below the 8-char floor', () => {
    expect(extractApiKeyImportEntriesFromText('abc', 'serpapi')).toEqual([])
    expect(extractApiKeyImportEntriesFromText('abc,1.2.3.4', 'serpapi')).toEqual([])
  })

  it('keeps an omitted provider on the tavily path', () => {
    const text = ['a tvly-dev-first 1.1.1.1', 'skip me', 'b;tvly-dev-second;8.8.4.4'].join('\n')
    // Pre-change behavior, pinned literally: no provider argument === tavily.
    expect(extractApiKeyImportEntriesFromText(text)).toEqual([
      { api_key: 'tvly-dev-first', registration_ip: '1.1.1.1' },
      { api_key: 'tvly-dev-second', registration_ip: '8.8.4.4' },
    ])
    expect(extractApiKeyImportEntriesFromText(text)).toEqual(
      extractApiKeyImportEntriesFromText(text, 'tavily'),
    )
  })

  it('does not extract a native-shaped key when the provider is omitted', () => {
    expect(extractApiKeyImportEntriesFromText(SERPAPI_KEY)).toEqual([])
    expect(extractApiKeyImportEntryFromLine(SERPAPI_KEY)).toBeNull()
  })

  it('keeps the legacy tvly-dev wrappers on the tavily default', () => {
    expect(extractTvlyDevApiKeyFromLine('tvly-dev-wrap01 1.1.1.1')).toBe('tvly-dev-wrap01')
    expect(extractTvlyDevApiKeysFromText('tvly-dev-a\ntvly-dev-b')).toEqual([
      'tvly-dev-a',
      'tvly-dev-b',
    ])
    expect(extractTvlyDevApiKeysFromText(SERPAPI_KEY)).toEqual([])
  })
})
