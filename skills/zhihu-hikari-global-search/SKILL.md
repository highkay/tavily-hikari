---
name: zhihu-hikari-global-search
description: Search the wider web with Zhihu's global index through Tavily Hikari.
---

# Zhihu Hikari Global Search

Use this skill when a task needs web-wide Chinese search from Zhihu's global index through a
Tavily Hikari deployment. For zhihu.com content specifically, prefer `zhihu-hikari-search`.

## Workflow

1. Shape a focused keyword. Prefer this route when the answer is not confined to zhihu.com.
2. Run a native GET with bearer auth:

   ```bash
   ORIGIN="https://<your-hikari-host>"
   curl -s "$ORIGIN/api/zhihu/api/v1/content/global_search?Query=咖啡冲煮&Count=5" \
     -H "Authorization: Bearer th-<id>-<secret>"
   ```

3. Read the native envelope `{Code, Message, Data}`. `Data.Items[]` holds results with `Title`,
   `ContentText` (highlights use `<em>`), `Url`, `AuthorName`, `CommentCount`, `VoteUpCount`,
   and `RankingScore`. Treat `Code != 0` as a failure even though the HTTP status is 200.
4. Refine with `SearchDB=realtime` or `SearchDB=static`, or with `Filter`, URL-encoded. The
   filter grammar supports `host=="example.com"`, `publish_time>=1700000000`, uppercase
   `AND`/`OR` with `AND` binding tighter, and parentheses. `zhihu.com` hosts are unsupported in
   `Filter`; use `zhihu-hikari-search` for those.

## Boundaries

- `Count` is capped at 20. More than 20 returns a local 400 with zero upstream calls; `Count <= 0`
  passes through and the upstream default applies.
- `Query` is required and non-empty. `SearchDB` and `Filter` pass through unvalidated.
- This is the only allowed auth shape: `Authorization: Bearer th-...`. Zhihu has no
  client-native header or query slot for the token.

## Hikari Notes

- Base URL: `https://<your-hikari-host>/api/zhihu` mirrors `https://developer.zhihu.com`.
- Billing: 1 credit only when `Code == 0`, per the shared wallet. Non-zero codes settle 0 while
  the client still receives the envelope.
- Hikari injects the pooled Zhihu Access Secret upstream with a server-generated
  `X-Request-Timestamp`; client-supplied auth headers and timestamps are never forwarded.
- Operator note: the Access Secret is an account-level credential. Don't rotate across personal
  accounts to farm free quota, and verify the Zhihu open-platform resale terms before any public
  offering.
- MCP clients can use `https://<your-hikari-host>/zhihu/mcp` with the
  `zhihu_global_search` tool; see `zhihu-hikari-mcp`.
