# 上游合并记录

记录每次从上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch) 合并的详情。

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
