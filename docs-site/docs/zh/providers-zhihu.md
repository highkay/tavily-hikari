# Zhihu 供应商

Zhihu 开放平台提供站内内容搜索（`zhihu_search`）与全网搜索（`global_search`），统一用
HTTP-200 的 `Code` envelope 返回结果。Hikari 将两者包装成原生 GET facade，并提供本地 MCP
facade，credit 从共享钱包结算。

## 一览

| 项目        | 值                                                                        |
| ----------- | ------------------------------------------------------------------------- |
| REST base   | `https://<your-host>/api/zhihu`（上游默认 `https://developer.zhihu.com`） |
| REST 鉴权槽 | `Authorization: Bearer th-...`（无客户端原生槽）                          |
| MCP 入口    | `https://<your-host>/zhihu/mcp`（本地 facade）                            |
| MCP 工具    | `zhihu_search`、`zhihu_global_search`                                     |
| Credit      | 每个 `Code == 0` 响应计 1                                                 |
| 配额        | 从免费 `/api/v1/quota` 做每日主动同步                                     |
| 恢复        | `RemainingQuota = 0` 耗尽到下一个自然日                                   |

## REST 端点

`GET /api/zhihu/api/v1/content/zhihu_search` 接受 `Query`（必填、非空）、`Count`（默认 10，
上限 10）与 `SortBy`（`field:direction:(min,max)`）。`Count` 超过 10 会返回本地 400 且零上游
调用；`Count <= 0` 透传并应用上游默认值。

`GET /api/zhihu/api/v1/content/global_search` 接受 `Query`（必填）、`Count`（默认 10，上限
20，同样有本地保护）、`SearchDB`（默认 `all`，可选 `realtime`、`static`）与 `Filter`（`AND`/
`OR` 必须大写，`AND` 优先级高于 `OR`，括号分组）。

两个端点都返回原生 `{Code, Message, Data}` envelope：`0` 成功，`10001` 参数错误，`20001`
鉴权失败，`30001` 限速，`90001` 内部错误，未知非零 code 按客户端错误处理。只有
`Code == 0` 才计 1 credit。

上游鉴权使用池内 Access Secret 装入 `Authorization: Bearer`，并带上服务端生成的
`X-Request-Timestamp`；客户端自带的鉴权头与时间戳一律不转发。

## MCP

`/zhihu/mcp` 是本地 facade。供应商官方 MCP 仅支持 SSE，本项目不使用它。响应以 SSE 包裹；
用 GET 打开事件流返回 405，未知工具返回 400。

- `zhihu_search`：`query` 必填；`count` 1..10；`sort_by` 可选
- `zhihu_global_search`：`query` 必填；`count` 1..20；`search_db` all/realtime/static；
  `filter` 可选
- `initialize`、`ping`、`tools/list`、`notifications` 免费；`tools/call` 仅在翻译后的 REST
  结果为 `Code == 0` 时计 1 credit，否则返回 MCP 工具错误且计 0

## 限制与配额语义

- Count 上限：站内搜索 10，全网搜索 20
- 每日配额来自免费 `/api/v1/quota` 调用；Hikari 一次读取两个 API，保存两者中较小的
  `RemainingQuota`，并记录下一个自然日（Asia/Shanghai 零点）的恢复时间
- `RemainingQuota = 0` 时 key 耗尽到该自然日边界
- 配额条目缺失是 typed 同步错误，绝不静默记 0

## 零计费校验

管理端 key 校验调用 `/api/v1/quota`：`Code = 0` 表示有效，`20001` 表示无效。该配额调用不
消耗业务额度。计费式校验探针被禁止。

## 环境变量

| Env var          | 默认值                        | 含义                             |
| ---------------- | ----------------------------- | -------------------------------- |
| `ZHIHU_API_KEYS` | 未设置                        | 逗号分隔的引导导入 Access Secret |
| `ZHIHU_UPSTREAM` | `https://developer.zhihu.com` | REST 上游基地址                  |

对应的 CLI 参数是 `--zhihu-keys` 与 `--zhihu-upstream`。没有单独的 MCP upstream 变量，因为
`/zhihu/mcp` 由 Hikari 本地提供。

## 运营者条款注记

Zhihu Access Secret 是账号级凭据。Hikari 只通过共享钱包向终端用户提供 Zhihu，不自动执行
多账号轮换或免费档刷量。任何公开出售前，运营者必须核对当前套餐是否允许通过 Zhihu 开放
平台条款进行转售。

## 运营注记

- 部署出口：部署时从目标网络确认 `developer.zhihu.com` 在传输层可达。这是运营者 runbook
  步骤，永远不进 CI。
- 测试纪律：测试与开发脚本只使用 Hikari 的 stub 上游（`mock_zhihu`），永远不要指向生产
  端点。
