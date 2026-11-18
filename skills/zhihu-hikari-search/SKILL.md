---
name: zhihu-hikari-search
description: Search Zhihu in-site content through Tavily Hikari.
---

# Zhihu Hikari Search

Use this skill when a task needs Chinese content from zhihu.com itself, such as answers,
articles, or discussions, through a Tavily Hikari deployment.

## Workflow

1. Shape a focused keyword. This route searches zhihu.com content only; for the wider web use
   `zhihu-hikari-global-search`.
2. Run a native GET with bearer auth:

   ```bash
   ORIGIN="https://<your-hikari-host>"
   curl -s "$ORIGIN/api/zhihu/api/v1/content/zhihu_search?Query=咖啡冲煮&Count=5" \
     -H "Authorization: Bearer th-<id>-<secret>"
   ```

3. Read the native envelope `{Code, Message, Data}`. `Data.Items[]` holds the results with
   `Title`, `ContentText`, `Url`, `AuthorName`, `CommentCount`, `VoteUpCount`, `EditTime`,
   `CreateTime`, and `RankingScore`. Treat `Code != 0` as a failure even though the HTTP status
   is 200.
4. Order results with `SortBy=field:direction:(min,max)`, such as
   `VoteUpCount:desc:(100,)` or `EditTime:asc:(,172800)`.

## Boundaries

- `Count` is capped at 10. More than 10 returns a local 400 with zero upstream calls; `Count <= 0`
  passes through and the upstream default of 10 applies.
- `Query` is required and non-empty. `SortBy` passes through unvalidated.
- This is the only allowed auth shape: `Authorization: Bearer th-...`. Zhihu has no
  client-native header or query slot for the token.

## Hikari Notes

- Base URL: `https://<your-hikari-host>/api/zhihu` mirrors `https://developer.zhihu.com`.
- Billing: 1 credit only when `Code == 0`, per the shared wallet. `10001` (parameter), `20001`
  (auth), `30001` (rate limit), `90001` (internal), and unknown codes settle 0.
- Hikari injects the pooled Zhihu Access Secret upstream with a server-generated
  `X-Request-Timestamp`; client-supplied auth headers and timestamps are never forwarded.
- Operator note: the Access Secret is an account-level credential. Don't rotate across personal
  accounts to farm free quota, and verify the Zhihu open-platform resale terms before any public
  offering.
- MCP clients can use `https://<your-hikari-host>/zhihu/mcp` with the `zhihu_search` tool;
  see `zhihu-hikari-mcp`.
