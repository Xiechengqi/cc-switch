# 上游合并记录

记录每次从上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch) 合并的详情。

---

## 2026-04-24

- **上游分支：** `main`
- **上游 HEAD：** `c002688a`（`v3.14.1`）
- **共同祖先：** `c7ba3cf5`
- **合并提交数：** 14
- **主要变更：**
  - feat(tray): 新增 Kimi / Zhipu / MiniMax 的 coding-plan 用量展示；托盘菜单可缓存并展示各供应商配额（引入 `services/usage_cache.rs` 与 `hooks/useUsageCacheBridge.ts`）
  - refactor(hermes): 删除 `HermesHealthBanner`、`scan_hermes_config_health` 命令与 `useHermesHealth` hook，配置 health scanner 不再维护
  - feat(codex): 新增 Codex OAuth FAST mode（发送 `service_tier="priority"` 降低延迟，可在 `CodexOAuthSection` 中切换），`ProviderMeta.codex_fast_mode` 字段随之入库
  - feat(codex): 稳定 Codex OAuth 缓存路由 —— 透传客户端提供的 `session_id` 到上游并补充 `x-client-request-id` / `x-codex-window-id` header；非流式请求遇到 Codex OAuth SSE 时聚合为 Responses JSON 再走非流式转换
  - fix(proxy): Codex OAuth 响应路径统一走流式兜底（`ProxyResponse::is_sse()` 判断 + 异常回放）
  - fix(skill): 通过 `resolve_root_level_repo_skills` 解决 root-level repo skills 一致性；修复 import 按钮连点导致重复导入的问题
  - feat(codex): TOML parser 替代正则抽取 Codex 模型（#2222）
  - fix(provider): "一键配置" 失效回归（#2249）
  - fix(gemini-cli): resume session 携带 `project_dir`（#2240）
  - chore(release): 版本号 bump 到 3.14.1
- **冲突解决（Rust 后端）：**
  - `src-tauri/src/provider.rs` — 保留本仓 OAuth 判定族（`is_codex_oauth_provider` / `is_claude_oauth_provider` / `is_codex_official_with_managed_auth` / `is_managed_oauth_provider` / `supports_stream_check` / `stream_check_base_url_override` 等），新增上游的 `is_codex_oauth`（代理到 `is_codex_oauth_provider`）、`codex_fast_mode_enabled`、`has_usage_script_enabled`
  - `src-tauri/src/proxy/forwarder.rs` — 保留本仓 OAuth 401 单次重试 `loop` 结构与 Claude OAuth 动态 token 注入；在 loop 内部加入 `should_send_codex_oauth_session_headers` 标志位（CodexOAuth token 获取成功时置 true）并在 body 序列化前注入 `build_codex_oauth_session_headers` 产出的三个 header；测试保留本仓 `anthropic_beta_*` 三个用例并补充上游 `codex_oauth_session_headers_match_codex_cache_identity`
  - `src-tauri/src/proxy/handlers.rs` — 非流式响应解析兼容 `aggregate_codex_oauth_responses_sse`，保留本仓 `upstream_response_for_usage = upstream_response.clone()` 供 usage 统计回退
  - `src-tauri/src/proxy/hyper_client.rs` — 自动合并漏掉了 `ProxyResponse::is_sse()` 方法，手动补齐（与上游实现一致）
  - `src-tauri/src/services/subscription.rs` — `KNOWN_TIERS` 合并：保留本仓 `"seven_day_omelette"`（Anthropic endpoint 历史兼容），其余四项采用上游常量
  - `src-tauri/src/services/usage_cache.rs` / `src-tauri/src/tray.rs` — 新建 `SubscriptionQuota` 时补齐本仓新增字段 `failure: None`
  - `src-tauri/src/store.rs` — `AppState` 同时持有本仓 `tunnel_manager: Arc<RwLock<TunnelManager>>` 和上游 `usage_cache: Arc<UsageCache>`
- **冲突解决（前端）：**
  - `src/App.tsx` — 移除上游已删除的 `HermesHealthBanner` import（auto-merge 已清理 `useHermesHealth` / `hermesKeys.health` 失效调用与 banner 渲染），保留本仓 `SharePage` import 与 upstream 新引入的 `useUsageCacheBridge`
  - `src/components/providers/forms/ClaudeFormFields.tsx` / `ProviderForm.tsx` — 同时接入本仓 `isClaudeOauthPreset` / `selectedClaudeAccountId` / `onClaudeAccountSelect` 与上游 `codexFastMode` / `onCodexFastModeChange`
  - `src/components/providers/forms/CodexOAuthSection.tsx` — Props 合并：保留本仓 `allowDefaultAccountOption` / `showLoggedInAccounts` 与上游 `fastModeEnabled` / `onFastModeChange`；布局采用本仓顺序（FAST mode 开关放在已登录账号列表之前，账号选择器延后），删除上游重复的早期账号选择器块
- **其余冲突：** Git 三方合并自动处理 10+ 个文件（`Cargo.toml/lock`、`package.json`、`lib.rs`、`providers/claude.rs`、`services/stream_check.rs`、`handler_context.rs`、i18n JSON 等）
- **验证：** `cargo check --lib`、`cargo check --bins --tests`、`cargo test --lib`（1023 通过）、`cargo test --test hermes_roundtrip --test skill_sync --test import_export_sync --test mcp_commands`（24 通过）、`pnpm typecheck`、`pnpm test:unit`（259 通过）

---

## 2026-04-22

- **上游分支：** `main`
- **上游 HEAD：** `c7ba3cf5`
- **共同祖先：** `c5b15dd2`
- **合并提交数：** 60
- **主要变更：**
  - feat(hermes): 全新引入 Hermes Agent 作为第 6 个支持的应用（Phase 1–8）
    - 新增 `src-tauri/src/hermes_config.rs`、`commands/hermes.rs`、`mcp/hermes.rs`、`session_manager/providers/hermes.rs`
    - 数据库迁移新增 `enabled_hermes` 列（mcp_servers、skills）
    - 新增前端 `components/hermes/HermesHealthBanner.tsx`、`HermesMemoryPanel.tsx`、`HermesFormFields.tsx`、`useHermesFormState.ts` 等
    - 统一 Skills 管理支持 Hermes、Usage 查询弹窗支持 Hermes 与 OpenClaw
  - feat(presets): Claude Opus 4.7 全面替换聚合器/Bedrock presets、加入自适应思考与 Bedrock SKU
  - feat(presets): 新增 LemonData（六个应用）、DDSHub Codex、Kimi K2.6 升级
  - feat(copilot): 转发前剥离 thinking blocks 以节省 premium 配额
  - feat(claude): effort 切换上限从 "high" 提升为 "max"
  - fix(header): 最大化后 auto-compact 不再保持锁定
  - fix(providers): Claude quick-set 移除过时的 `ANTHROPIC_REASONING_MODEL`
  - chore(release): 版本号 bump 到 3.14.0
- **冲突解决（数据库 schema 版本号冲突）：**
  - `src-tauri/src/database/mod.rs` — 上游把 `SCHEMA_VERSION` 升到 10，本仓已经到 15（Token 分享相关 v9→v15 迁移）。采用把上游的 Hermes 迁移挂在本仓链尾的方案：新增 `migrate_v15_to_v16`（添加 `enabled_hermes` 列），`SCHEMA_VERSION` 升到 16
  - `src-tauri/src/database/schema.rs` — 保留本仓 v9→v15 的全部迁移函数，把上游的 v9→v10（Hermes 列）重命名为 `migrate_v15_to_v16`；match 表新增 `15 =>` 分支
- **冲突解决（前端 import 与 helper 重构）：**
  - `src/App.tsx` — import 块合并：同时保留本仓 `SharePage` 与上游 Hermes 组件
  - `src/components/providers/ProviderCard.tsx` — 上游引入 `isCopilot` / `isCodexOauth` 内联判定替代本仓的 `isManagedOauthProvider` helper，同时移除 helper 导入。保留本仓 `isManagedOauthProvider` / `isOfficialBlockedByProxyTakeover` / `ClaudeOauthQuotaFooter` / `canTestProvider`，只拉入上游的 `isHermesReadOnly`（被 `isReadOnly={isHermesReadOnly}` 用到）。丢弃上游未用到的 `isCopilot` / `isCodexOauth`（本仓已由 helper 覆盖）
  - `src/components/providers/forms/ProviderForm.tsx` — import 块合并：保留本仓 `PROVIDER_TYPES`、拉入上游 `useHermesLiveProviderIds`
  - `tests/msw/state.ts` — `LiveProviderIdsByApp` 合并为 `"opencode" | "openclaw" | "hermes"`，保留本仓 `ShareConnectInfo`
- **其余冲突：** Git 三方合并自动处理（约 120 个文件）
- **验证：** `pnpm typecheck`、`pnpm test:unit`（254 通过）、`cargo check`、`cargo test --lib`（995 通过）、`cargo test --test hermes_roundtrip`（2 通过）、`cargo test --test skill_sync --test import_export_sync --test mcp_commands`（24 通过）。其余 integration 测试因主机磁盘满（`/dev/mapper/ubuntu--vg-ubuntu--lv` 100%）导致 `ld` 链接阶段 bus error，非合并问题

---

## 2026-04-21

- **上游分支：** `main`
- **上游 HEAD：** `c5b15dd2`
- **共同祖先：** `1126c745`
- **合并提交数：** 9
- **主要变更：**
  - feat(copilot): 新增 GitHub Enterprise Server 支持 (#2175) — `GitHubAccount` 增加 `github_domain` 字段，`useCopilotAuth` 支持传入 domain
  - feat(ui): 模型映射字段新增快速设置按钮 (#2179)
  - fix(skills): 导入 skills 后同步到应用目录 (#2101)
  - Add OpenClaw config directory settings (#1518)
  - Fix Ghostty session restore launch path (#1976)
  - fix(tray): 使用应用专属 tray id (#1978)
  - Add StepFun and StepFun en Step Plan presets (#2155)
  - fix: Codex/Claude/Gemini 公共配置复选框状态持久化 (#2191)
  - fix(claude-plugin): 当前 provider 配置同步到 settings.json (#1905)
- **冲突解决：**
  - `src/components/providers/forms/CopilotAuthSection.tsx` — 合并上游 GitHub Enterprise 部署选择器与 `github_domain` 徽章，保留本仓 `showLoggedInAccounts` prop 以及位置前置的已登录账号列表；`useCopilotAuth(effectiveGithubDomain)` 保留本仓解构的 `removeAccount` / `setDefaultAccount`
  - `src-tauri/src/proxy/providers/claude_oauth_auth.rs` — `GitHubAccount` 结构体新增 `github_domain` 字段导致本仓的 Claude OAuth 账号 `From<&ClaudeAccountData>` 初始化缺字段，补齐为默认值 `"github.com"`
- **其余冲突：** Git 三方合并自动处理（`src-tauri/src/commands/auth.rs`、`lib.rs`、`proxy/providers/codex_oauth_auth.rs`、`proxy/providers/copilot_auth.rs`、`tray.rs`、`src/components/providers/forms/ClaudeFormFields.tsx`、`src/i18n/locales/{en,ja,zh}.json`、`src/lib/api/auth.ts`、`src/lib/schemas/settings.ts` 等 31 个文件）
- **验证：** `pnpm typecheck`、`pnpm test:unit`（247 通过）、`cargo check`、`cargo test --lib copilot_auth`（20 通过）

---

## 2026-04-18

- **上游分支：** `main`
- **合并提交：** `8a87b35c`
- **上游 HEAD：** `1126c745`
- **共同祖先：** `de23216e`
- **合并提交数：** 2
- **主要变更：**
  - feat(proxy): Gemini Native API proxy integration (#1918)
    - 新增 `proxy/gemini_url.rs`、`providers/gemini_schema.rs`、`providers/gemini_shadow.rs`
    - 新增 `providers/streaming_gemini.rs`、`providers/transform_gemini.rs`
    - Claude adapter 支持 `gemini_native` api_format 与 Gemini / GeminiCli provider 类型
    - forwarder 根据 api_format 选择 `resolve_gemini_native_url` 构造 URL
  - style: 新增 `provider.notes` 字段 (#2138)
- **冲突解决：**
  - `src-tauri/src/proxy/forwarder.rs` — 同时保留本地 Claude OAuth 逻辑（`ensure_claude_oauth_beta_query`、`sign_claude_oauth_messages_body`、动态 access_token 注入、`is_claude_oauth_provider`）和上游 `resolve_gemini_native_url`
  - `src-tauri/src/proxy/providers/claude.rs` — 同时保留本地 `ProviderType::ClaudeOAuth` / `AuthStrategy::ClaudeOAuth` 分支与上游 Gemini/GeminiCli 检测及 `Google` / `GoogleOAuth` 认证策略
- **合并后整理（同日）：**
  - feat(share): 新增区域隧道路由与展示状态徽章（`fetch-regions.mjs`、`shareRegions.ts`、`ShareRouterBar`、`ShareDisplayStatusBadge`）
  - feat(oauth): 新增可配置的配额刷新间隔与 Claude OAuth 配额展示 footer

---

## 2026-04-16

- **上游分支：** `main`
- **合并提交：** `dfbe9277`
- **共同祖先：** `de23216e`
- **合并提交数：** 1
- **主要变更：**
  - feat(usage): 优化使用量仪表盘 UI，新增日期范围选择器 (#2002)
    - 新增 `UsageDateRangePicker` 组件，支持预设范围（今天/1d/7d/14d/30d）和自定义日期范围
    - 重构使用量查询 hooks，使用 `UsageRangeSelection` 替代旧的 `TimeRange` 类型
    - 新增 `usageRange.ts` 工具函数
    - 优化 `RequestLogTable` 布局，合并缓存/倍率列
    - 后端新增 `compute_rollup_date_bounds()` 对齐本地日期边界
    - 新增 `RequestLogTable` 回归测试
- **冲突解决：**
  - 无实际冲突 — Git 三方合并自动解决了所有 6 个共同修改的文件
  - `src-tauri/src/services/usage_stats.rs` — 上游新增日期边界逻辑，本地新增 share 字段，互不影响
  - `src/lib/query/usage.ts` — 上游重构为 range preset 模式，本地 share_id 改动位于不同区域
  - `src/types/usage.ts` — 上游更新时间类型，本地新增 share 字段
  - `src/i18n/locales/{en,ja,zh}.json` — 上游新增 usage preset 国际化，本地新增 share 国际化

---

## 2026-04-15

- **上游分支：** `main`
- **合并提交：** `c993972f`
- **共同祖先：** `449a1712`
- **合并提交数：** 16
- **主要变更：**
  - feat(stream-check): 刷新默认模型列表，检测 model-not-found 错误
  - fix(proxy): 按 RFC 7230 剥离逐跳响应头
  - fix(opencode): 使用 json5 解析器兼容尾逗号
  - perf: 虚拟化会话消息列表，折叠长消息以降低渲染开销
  - refactor: 将 "Local Proxy Takeover" 重命名为 "Local Routing"
  - refactor: 移除按供应商单独配置代理的功能
- **冲突解决：**
  - `src-tauri/src/services/stream_check.rs` — 保留我方新增的 Anthropic header 和 connection keep-alive
  - `src/i18n/locales/{en,ja,zh}.json` — 采用上游的 routing 重命名和端口 15721
