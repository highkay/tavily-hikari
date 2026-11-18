# Keenable 供应商

Keenable 提供网页搜索与页面抓取 API，并带有托管 MCP 服务。通过 Hikari，同一枚 `th-` token
可以走它的原生 REST facade 与托管 MCP 直通入口，credit 从共享钱包结算。

## 一览

| 项目        | 值                                                                       |
| ----------- | ------------------------------------------------------------------------ |
| REST base   | `https://<your-host>/api/keenable`（上游默认 `https://api.keenable.ai`） |
| REST 鉴权槽 | `X-API-Key: th-...` 或 `Authorization: Bearer th-...`                    |
| MCP 入口    | `https://<your-host>/keenable/mcp`（托管直通）                           |
| MCP 工具    | `search_web_pages`、`fetch_page_content`                                 |
| Credit      | 成功 search、fetch 或 MCP `tools/call` 各计 1                            |
| 配额        | 被动采集 `usage.credits_remaining`                                       |
| 恢复        | `402` 耗尽到下一个 UTC 月或管理员手动恢复                                |

## REST 端点

`POST /api/keenable/v1/search` 原样透传 JSON body。常用参数：`query`（必填）、`mode`（默认
`pro`，可选 `realtime`）、`max_results` 1..50（默认 10）、`site`、`exclude_domains`、
`published_after` 与 `published_before`、`acquired_after` 与 `acquired_before`、`query_time`、
`snippet_max_length` 180..10000、`safety_mode`、`disable_cards`、`session_id`。

`GET /api/keenable/v1/fetch` 原样透传 query string。常用参数：`url`（必填）、`max_chars`
（默认 50000）、`live`、`prompt`（最多 2000 字符）、`session_id`。

响应体在凭据脱敏后原样透传；带 key 的成功响应携带 `usage`。

## MCP

`/keenable/mcp` 是对供应商托管 MCP 服务的缓冲直通（Streamable HTTP、JSON-RPC 2.0，无 SSE）。
鉴权：`X-API-Key: th-...` 或 bearer。

- `initialize`、`ping`、`notifications/*`、`tools/list`：免费
- 使用 `search_web_pages` 或 `fetch_page_content` 的 `tools/call`：成功计 1 credit
- 成功的工具调用携带 `_meta["keenable/usage"]` 与 `credits_remaining`；Hikari 会采集它

`search_web_pages` 接受 `query`、`site`、`exclude_domains`、日期过滤、`max_results` 1..50、
`snippet_max_length`、`mode`、`output_format`、`session_id`、`disable_cards`。
`fetch_page_content` 接受 `url`、`max_chars`、`live`、`prompt`、`session_id`、`output_format`。

## 限制与配额语义

- 每组织 10 请求/秒，由该组织全部 key 及 search 与 fetch 共享
- 合同签署时供应商公布的免费档为 100,000 请求/月，之后为每 1,000 请求 4 美元（同为合同
  签署时公布）；套餐可能变化，实际定价请以供应商当前公布为准
- `usage.credits_remaining` 是可能有延迟的估算值；Hikari 在每个带 key 的 REST 成功响应与
  MCP `tools/call` 中采样，仅在材料变化时落库
- 管理端 `sync-usage` 返回最后一次被动快照；Keenable 没有主动轮询
- `402` 让 key 耗尽到下一个 UTC 月边界或管理员手动恢复；`401` 进入 quarantine；`429` 遵守
  `Retry-After`；`target_error` 属于客户端错误，绝不标记 key

## 零计费校验

管理端 key 校验发送一个空 body 的搜索请求（缺少 `query`）：`400 missing_parameter` 表示 key
有效，`401` 表示无效，其他状态是 typed 原因。该探针不产生任何计费。计费式校验探针被禁止。

## 环境变量

| Env var                 | 默认值                        | 含义                   |
| ----------------------- | ----------------------------- | ---------------------- |
| `KEENABLE_API_KEYS`     | 未设置                        | 逗号分隔的引导导入 key |
| `KEENABLE_UPSTREAM`     | `https://api.keenable.ai`     | REST 上游基地址        |
| `KEENABLE_MCP_UPSTREAM` | `https://api.keenable.ai/mcp` | 完整的托管 MCP 端点    |

对应的 CLI 参数是 `--keenable-keys`、`--keenable-upstream` 与 `--keenable-mcp-upstream`。

## 运营注记

- 部署出口：启用 Keenable key 之前，先从目标网络确认 `api.keenable.ai` 在传输层可达。这是
  运营者 runbook 步骤，永远不进 CI。
- 测试纪律：测试与开发脚本只使用 Hikari 的 stub 上游（`mock_keenable`），永远不要指向生产
  端点。
