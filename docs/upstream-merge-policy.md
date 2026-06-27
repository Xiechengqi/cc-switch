# 上游合并策略

## 基本判断

cc-switch 本 fork 不能完全停止跟进上游。Claude、Codex、Gemini 账号反代能力仍处在高频变化期，上游提交经常直接影响请求转换、OAuth、streaming、tool call 和用量统计。

正确策略是：继续合并或选择性吸收上游核心反代修复，同时把本地 share/router/market/web 能力隔离，降低合并冲突。

## 必须重点关注的上游路径

以下路径的上游改动通常和账号反代稳定性相关，需要优先审查：

- `src-tauri/src/proxy/**`
- `src-tauri/src/provider.rs`
- `src-tauri/src/services/provider/**`
- `src-tauri/src/services/subscription.rs`
- `src-tauri/src/services/coding_plan.rs`
- `src-tauri/src/services/usage_stats.rs`
- `src-tauri/src/database/schema.rs`
- `src/config/*ProviderPresets.ts`
- `src/config/codingPlanProviders.ts`

## 高价值上游改动类型

优先吸收这些类型的改动：

- Claude/Codex/Gemini 官方账号反代修复
- OAuth token、refresh token、auth header、全局代理相关修复
- Codex Responses API、Chat API、streaming 转换
- Claude tool use、thinking、system message、cache 字段处理
- 请求 body 解压、zstd、content-encoding、转发兼容
- 模型映射、actual model、usage/token/cost 统计
- 官方模型、定价、订阅额度查询逻辑

## 低优先级上游改动类型

这些改动可以合并，但不应为了它们破坏本地架构边界：

- README、release notes、营销文案
- provider preset referral 链接
- about 页面、banner、视觉展示
- 与当前产品不聚焦的 UI 重排

## 不建议的做法

不要直接停止 upstream merge。这样短期省事，但 Claude/Codex/Gemini CLI 或官方 API 变化后，账号反代会逐渐失效。

不要整块删除上游功能来做“清理”。删除 MCP、skills、session manager、updater 等功能会导致后续每次合并都出现大量冲突。

不要把本地 share/router/market 行为散落到上游高频 proxy/provider 文件。必要时通过小型 hook、metadata 或独立服务层接入。

## 推荐合并流程

1. 先查看上游最近提交是否触及 proxy/provider/OAuth/usage/database。
2. 对协议修复类提交优先合并或 cherry-pick。
3. 对 UI/文案类提交正常合并，但避免本地产品方向被上游 UI 牵动。
4. 如果冲突发生在本地扩展层之外，优先思考能否把本地逻辑迁回 `local_ext`、`web`、`tunnel` 或 `services/share.rs`。
5. 合并后运行基础验证：
   - `cargo fmt --check`
   - `cargo check --offline`
   - `npm run typecheck`
   - `cargo test --offline --lib`
