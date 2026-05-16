# 上游合并记录

记录每次从上游 [farion1231/cc-switch](https://github.com/farion1231/cc-switch) 合并的详情。

---

## 2026-05-20

- **上游分支：** `main`
- **上游 HEAD：** `6172bfd5`
- **共同祖先：** `0d095555`
- **合并提交数：** 20
- **合并提交：** `11d969b2`
- **新增 upstream tag：** `v3.15.0`
- **主要变更：**
  - chore(release): 上游 bump `tauri.conf.json` 到 3.15.0；新增 `docs/release-notes/v3.15.0-{en,ja,zh}.md`（含 imposter site warning 措辞）
  - feat(failover): P1–P3 reliability gaps 修复（b642ef06）—— 新增 `prepare_success_response_for_failover` + `prime_streaming_response`，对应 `ProxyResponse::buffered/streamed` 构造器
  - feat(providers): Claude Code / Codex 路由支持徽章（940161fb）
  - feat(presets): 新增 PatewayAI / ClaudeAPI / ClaudeCN / RunAPI / RelaxyCode / 火山 Agentplan / BytePlus 等合作商 preset；DouBaoSeed endpoint 更新；20 个 Claude Desktop preset 从 proxy mode 切到 direct mode；preset 渲染顺序按数组顺序、合作商优先
  - chore(i18n): `modelMappingOffHint` 改为 action-oriented 文案
- **冲突解决：**
  - `src-tauri/src/proxy/hyper_client.rs` — 上游把 `Local/LocalStream` 重命名为 `Buffered/Streamed`，并新增带 status/headers 入参的 `buffered/streamed` 构造器。因 `src-tauri/src/proxy/providers/deepseek_claude.rs` 等本仓代码仍在用 `local_json/local_sse + Local/LocalStream`，保留旧命名；同时**并行新增** `buffered/streamed` 构造器（内部仍走 `Local/LocalStream` 变体），让上游新引入的 `prepare_success_response_for_failover` / `prime_streaming_response` 可编译。两套 API 互不冲突。
  - `src-tauri/src/proxy/forwarder.rs` —
    - 上游 dispatch 重构第 4 次**未采纳**（pooled-reqwest + auth 提出 retry loop 与本仓 401 重试不兼容），`should_preserve_exact_header_case` / `map_reqwest_send_error` 继续是 dead-code warning
    - 上游 `if status.is_success() { prepare_success_response_for_failover(...) } else { ... }` 包裹层也**未采纳**——保留本仓 retry-loop 平铺路径；`prepare_success_response_for_failover` 方法仍在文件里保留（被上游测试用例引用），供日后做 failover 重构时直接接入
- **附加 dead-code warning：**
  - `prepare_success_response_for_failover` / `prime_streaming_response` / `ProxyResponse::buffered` / `ProxyResponse::streamed` 4 个未被生产代码引用，仅测试用，加上之前遗留的 `should_preserve_exact_header_case` / `map_reqwest_send_error` / `streaming_first_byte_timeout` 字段 / `_method`，共 7 条 warning
- **验证：** `cargo check`（7 warning，无 error）、`pnpm tsc --noEmit` 通过

---

## 2026-05-16

- **上游分支：** `main`
- **上游 HEAD：** `0d095555`
- **共同祖先：** `7685ab70`
- **合并提交数：** 35
- **合并提交：** `60d0149d`
- **主要变更：**
  - feat(claude-desktop): Claude Desktop 切换全面重做——Claude Code 导入流改造、role-based model mapping（sonnet/opus/haiku 角色锁定，1M flag 用 `supports1m` 字段而非 `[1M]` 后缀）、修复模型输入框失焦、恢复共享入口、加 Copilot/Codex OAuth 供应商支持（含 OAuth proxy 回归测试）
  - feat(codex-oauth): 按需从 ChatGPT 后端拉模型列表（新 `get_codex_oauth_models` 命令）
  - feat(proxy): 转发客户端实际 HTTP method（不再硬编码 POST，5d3d9067）；P0–P3 路由/生命周期一揽子修复（206125b4）、`max_retries` 接入 `AppProxyConfig`、`get_auth_headers` 改返回 `Result` 避免 panic（c9a6afc0）、failover 决策精修（cb4ecd39）、takeover 检测收紧 + 关闭时 fallback restore（c3d810a2）、从 URI path 正确提取 Gemini 模型（9a8f5202）、Anthropic tool_choice 映射到 OpenAI Chat 嵌套形式（84aa87c3）、IPv6 listen 地址校验放行（3c359725）、`auth_header_value` 跨 provider adapter 复用（039784af）、`handle_rectifier_retry_failure` helper 抽出（85131d37）、client-request 计数器移出 per-attempt loop（f2ae9823）、Claude Code 菜单暴露真实第三方模型名（f93b935d）、reuse pooled HTTPS for non-Anthropic（仍**未采纳**，见冲突解决）
  - fix(usage): 缓存成本语义修正 + 定价告警风暴静默（aa5e58d0）、pricing routing / SSE 生命周期 / 校验加固（402570ce）、按 model 精准回填零成本（新 `backfill_missing_usage_costs_for_model`）
  - feat(usage): filter-driven Hero + cache-normalized totals（`isUnpricedUsage`、`getFreshInputTokens`、`isNonNegativeDecimalString` 等导出）
  - fix(providers): 第三方 Claude 供应商禁用 model test（543e057e）
  - feat(ui): App switcher 区分 Claude Code 与 Claude Desktop；`ed41a7a7`、`968c75bd`；空 toolbar capsule 隐藏；Monitor badge 图标对齐居中
  - chore(presets/partners): OpenClaudeCode → MicuAPI 域名；CrazyRouter API endpoint 切到 cn 子域；移除 DDSHub 集成；BytePlus / Volcengine README 互链
- **冲突解决：**
  - `src-tauri/src/database/mod.rs` — 同时保留本仓 `ShareRecord` re-export 与上游新增 `validate_cost_multiplier` / `validate_pricing_source` / `PRICING_SOURCE_REQUEST` / `PRICING_SOURCE_RESPONSE`
  - `src-tauri/src/lib.rs` — invoke_handler 同时注册本仓 `get_claude_oauth_quota` / `get_cached_oauth_quota` 与上游 `get_codex_oauth_models`
  - `src-tauri/src/proxy/forwarder.rs` —
    - 保留本仓 `gemini_code_assist_model` 分支与 OAuth 401 retry `loop`；上游 e15bfbfe 的 pooled-reqwest + auth 提出 loop 重构第三次**未采纳**（解开 retry loop 会冲突我们的 401 重试），`should_preserve_exact_header_case` / `map_reqwest_send_error` 继续是 dead-code warning（也保留 `streaming_first_byte_timeout` 字段未读的 warning）
    - 序列化/日志/发送 dispatch 仍走本仓 `outbound_body`（Gemini Code Assist `project_id` 注入路径）
    - 采纳上游 c9a6afc0：`adapter.get_auth_headers(&auth)` 改回 `Result`，调用处补 `?`
  - `src-tauri/src/proxy/handler_context.rs` — 完整采纳上游 9a8f5202：`extract_gemini_model_from_endpoint` → `extract_gemini_model_from_path`，返回 `Option<String>`，更鲁棒；上游 8 个新测试用例完全覆盖原本仓 3 个旧用例
  - `src-tauri/src/proxy/response_processor.rs` — 采纳上游 206125b4 把 `connection_guard` 透传给 `handle_streaming`，但 SSE 判定保留本仓 `should_handle_as_streaming_response`（包含 SSE content-type **或** `ctx.request_is_streaming`），上游退回单纯 `is_sse_response` 不采用
  - `src-tauri/src/services/usage_stats.rs` — 采纳上游 `backfill_missing_usage_costs_for_model` + `only_model_id: Option<&str>` 参数 + `BASE_SQL` 常量化；SELECT 列保留本仓 `request_agent / requested_model / actual_model / actual_model_source / share_id / share_name`（`row_to_request_log_detail` 依赖这些索引）
  - `src/components/providers/ProviderCard.tsx` — 保留本仓 `canTestProvider(provider, appId)` 调用，把上游 543e057e 的"第三方 Claude 禁用 test"新增到 helper 内部（`src/utils/providerMetaUtils.ts`）而非内联，并清理冲突解决时引入但未引用的 `isCodexOauth` / `isClaudeThirdParty` locals
  - `src/components/providers/forms/ProviderForm.tsx` — 同时保留本仓 `hasManagedAuthBinding` import 与上游新增 `isNonNegativeDecimalString` import
  - `src/components/usage/RequestLogTable.tsx` — 套用上游 `logs.map((log) => { const unpriced = isUnpricedUsage(log); return ... })` 外壳（带 cache-normalized 输入 + 未定价提示），但内层模型 cell 仍是本仓 `requestAgent` 标签 + `requestedModel → actualModel` 三段式 display（share-aware）
- **验证：** `cargo check`（4 条 warning，含 3 条遗留 + 1 条 `_method` unused，无 error）、`pnpm tsc --noEmit` 通过

---

## 2026-05-11

- **上游分支：** `main`
- **上游 HEAD：** `7685ab70`
- **共同祖先：** `10c874af`
- **合并提交数：** 11
- **合并提交：** `6be6251b`
- **主要变更：**
  - perf(proxy): 减少每请求 hot-path 工作量与数据库等待
  - fix(proxy): 提升 Codex / Responses 请求的缓存命中率（新增 `prepare_upstream_request_body`，做私有参数过滤 + 键序 canonicalize；新增 `log_prompt_cache_trace` 用于调试 prompt cache 命中）
  - fix(proxy): Read tool 的输入中去掉空白页 (#2472)
  - fix(ci): 修复前端 formatting 与 Linux clippy
  - chore(brand): 把 `ccswitch.io` 立为唯一官方网站；release notes 模板里也展示
  - docs: 新增 RunAPI / ClaudeCN / 火山引擎（Volcengine）赞助商，对应 raster icons 资源
  - docs: 三语 README 标题副标补充 Hermes Agent
- **冲突解决：**
  - `.github/workflows/release.yml` — 完全保留本仓的 tauri-action 流程，丢弃上游的 legacy `publish-release` / `assemble-latest-json` job（本仓改造期已删除 `release-ci.yml`，新流程不需要这两个 job）
  - `src-tauri/src/proxy/forwarder.rs` —
    - body 预处理：采纳上游 `prepare_upstream_request_body`（替代 `filter_private_params_with_whitelist`，附带 key 排序），随后仍按本仓顺序套用 `normalize_codex_oauth_responses_body` / `sign_claude_oauth_messages_body`；最后调用上游新增的 `log_prompt_cache_trace`
    - 发送 dispatch：与 2026-05-08 同样**未采纳**上游 e15bfbfe 的 pooled-reqwest 路径（仍冲突本仓 401 retry loop），`should_preserve_exact_header_case` / `map_reqwest_send_error` 继续是 dead-code warning
    - 测试模块：保留本仓三个 `anthropic_beta_*` 用例与上游两个新用例 `canonical_json_sorts_object_keys_for_cache_trace_hashes` / `prepare_upstream_request_body_filters_private_fields_and_canonicalizes_order`
  - `src-tauri/src/proxy/handlers.rs` — 采纳上游 3 参数版 `SseUsageCollector::new(start_time, Some(claude_stream_usage_event_filter), |events, first_token_ms| ...)` 与 Some/None 包装，但闭包内部保留本仓 `share_id` / `share_name` / `incoming_request_id` / `schedule_sync_share_request_log` / `share_guard::record_share_request` 的完整 share 计费路径；顺手清掉外层一个被新作用域 shadow 的冗余 `let session_id`
  - `src-tauri/src/proxy/response_processor.rs` — 把上游的 `if !logging_enabled { return None }` 改为本仓语义 `let should_log_request = logging_enabled || ctx.share_id.is_some(); if !should_log_request { return None; }`，让纯 share 路径在日志关闭时仍能采集；闭包内用 `logging_enabled` 守 `log_usage_internal` 调用，`if let Some(sid) = share_id` 块保持无条件调度 share sync；同步采纳上游的 3 参数 `SseUsageCollector::new` + Some 包装
- **附加修复：**
  - `handlers.rs:573` 把 `Some(session_id)` 改为 `Some(session_id.clone())`，避免 share-log 同一作用域内 move 后再用导致 E0382
  - `handlers.rs:553` 删除 shadow 掉的旧 `let session_id = ctx.session_id.clone()`（unused 警告）
- **验证：** `cargo check`（3 条 dead-code warning，含上述 2 条遗留 + 1 条 `streaming_first_byte_timeout` field never read，无 error）、`pnpm tsc --noEmit` 通过

---

## 2026-05-08

- **上游分支：** `main`
- **上游 HEAD：** `10c874af`
- **共同祖先：** `1d44b1ba`
- **合并提交数：** 16
- **合并提交：** `07d0bc5a`
- **主要变更：**
  - feat(claude-desktop): 新增 44 个 provider preset，从 Claude Code 翻译过来；通过 proxy gateway 支持第三方 provider 切换；form UI 与 Claude Code 对齐；模型映射 UX 简化；3P provider 仅在需要路由时才展示徽章；移除 proxy-stopped 状态告警；`[1M]` 后缀 model route 匹配修复；裁剪 proxy / switch 流程冗余
  - fix(codex): 启动期 live import 重复处理 (#2590)
  - fix(proxy): 非 Anthropic 后端复用 pooled HTTPS 连接（**未采纳**，见冲突解决）
  - feat(deepseek): 工具调用响应携带 `reasoning_content` (#2543)
  - chore(backend): 通过 `cargo fmt` / `cargo clippy --all-targets`
  - docs: 新增 BytePlus、Micu API、Right Code、shengsuanyun、claudeapi 等赞助商展示，更新多个 logo / 文案
- **冲突解决：**
  - `.gitignore` — 保留本仓 `.github` 跟踪（CI workflow 在仓内维护，不能忽略），同时接受上游新加的 `mainWindow.js` 忽略项
  - `src-tauri/src/database/mod.rs` — 同时保留本仓 `pub use dao::shares::ShareRecord` 与上游新增 `pub(crate) use dao::providers_seed::CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID`
  - `src-tauri/src/proxy/forwarder.rs` —
    - 主体：保留本仓 retry `loop {}` 结构以及 OAuth 401 单次重试、Gemini Code Assist `outbound_body["project"] = project_id` 注入。**没有**采纳上游 `e15bfbfe` 的 pooled-reqwest 路径（`should_preserve_exact_header_case` + reqwest pool dispatch），因为它要求把外层 retry loop 拆成线性代码，与本仓的 401 重试和后续单包响应处理冲突；遗留为 TODO，编译期产出两条 `should_preserve_exact_header_case` / `map_reqwest_send_error` 未使用的 dead-code warning（保留是为下次评估时直接可用）
    - 测试模块：同时保留本仓新增的 `find_header_value` helper 与上游新增的 `test_provider_with_type` helper（两者都被各自的新测试用例使用）
  - `src-tauri/src/tray.rs` — 保留本仓 `crate::commands::switch_provider_internal` 调用（语义上等价于上游的 `crate::services::ProviderService::switch`，前者只是后者的薄包装），后接的 `OauthQuotaState` 切换后异步刷新逻辑也一并保留
- **附加：** 合并前先把工作区 62 个未提交文件 commit 为 `79cf62ba wip: local changes before upstream merge`（与 2026-05-02 同款流程），避免合并冲突解决期间被自动恢复
- **验证：** `cargo check`（仅 7 条 warning，含上述 2 条 dead-code）、`pnpm tsc --noEmit` 通过
- **注：** 推送命令 `git push origin main` 输出 `76da1f9d..07d0bc5a`

---

## 2026-05-02

- **上游分支：** `main`
- **上游 HEAD：** `72ab8a5c`
- **共同祖先：** `c002688a`
- **合并提交数：** 46
- **主要变更：**
  - feat(providers): 新增 Baidu Qianfan、Compshare（claude/codex/hermes/openclaw 四端 Coding Plan）、DeepSeek V4（flash/pro）等 preset
  - feat(model-fetch): `/models` 端点候选列表化（`build_models_url_candidates`），针对 Anthropic-compat 子路径供应商（DeepSeek 等）回退到根路径或剥离已知后缀
  - feat(copilot): Claude 模型 ID 通过 live `/models` 列表解析（不再硬编码）
  - feat(provider-form): "save anyway" 警告替代硬性校验（#2307）
  - feat(usage): 新增 Hermes Agent 用量追踪（一周内被回退：先 `f061b777` 加入，后 `518d945e` 撤销，仅保留间接 backfill 收益）；修复零成本与 proxy/session-log 重复计费
  - feat(tray): 托盘图标 tooltip
  - feat(window): 持久化 Tauri 窗口尺寸/位置（#2377）
  - feat(launcher): 支持 launch warp 并执行 session（#2466）
  - fix(proxy): 流式 `message_delta` 去重、tool calls scoped `reasoning_content`、preserve Vertex AI full URLs、include zero usage in final delta、strip leading billing header from system content
  - fix(claude): 通过环境变量名推导 Claude auth 策略（修复 ANTHROPIC env var 切换后 strategy 不更新）
  - fix(codex): 切换供应商后历史记录变化（#2349）、跳过 `environment_context` 提取 session title（#2439）、隐藏 Codex subagent sessions、隐藏 1M context window toggle
  - fix(linux): 主题选择导致 segfault（#2502）
  - fix(coding-plan): zhipu weekly tier 名按 reset time 矫正（#2420）
  - fix(dashscope): usage 解析鲁棒性（防 VSCode 崩溃 #2425）
  - fix(balance): SiliconFlow 国际站显示 USD（不再 CNY）
  - fix(config): JSON 键按字母排序（确定性输出 #2469）
  - fix(session): hide Codex subagent sessions、Codex usage log message
  - chore(codex): release 改进 `commands::try_get_version` 使用默认 shell（#2286）
  - chore(deps): GitHub Actions 升级（actions/checkout@6、softprops/action-gh-release@3、pnpm/action-setup@6、actions/stale@10）
  - chore(ci): 引入 Claude Code Action（review-only `@claude` 触发）、模型升级到 Opus 4.7
  - feat: import existing 改为 side-effect free（#2429）
- **冲突解决：**
  - `src-tauri/src/database/schema.rs` — 保留本仓 share_id index 注释，加入上游新增的 `create_request_logs_usage_indexes_if_supported(conn)?` 调用
  - `src-tauri/src/codex_config.rs` — 同时保留本仓 `PROXY_MODEL_PROVIDER_KEY`（"cc-switch"）与上游新增的 `CC_SWITCH_CODEX_MODEL_PROVIDER_ID`（"ccswitch"）/ `CODEX_RESERVED_MODEL_PROVIDER_IDS`
  - `src-tauri/src/services/model_fetch.rs` — 采用上游 `for url in &candidates` 候选列表回退结构，把本仓 share-tunnel `X-API-Key` header 注入逻辑挪进循环里（按 `is_share_tunnel_url(base_url)` 判断）
  - `src-tauri/src/services/usage_stats.rs` — 采纳上游抽出的 `row_to_request_log_detail` 共用 mapper，但更新它读取 `share_id`(23) / `share_name`(24) / `data_source`(25) 三列；同步更新 `backfill_missing_usage_costs_on_conn` 的 SQL 选列以匹配
  - `src/components/providers/forms/ProviderForm.tsx` — `handleSubmit` 与 `performSubmit` 两处都补回本仓 `isClaudeOauthProvider` 标识（`templatePreset/initialData.meta.providerType === "claude_oauth"`），用于 Claude OAuth 登录校验与 `authBinding.claude_oauth` 写入
- **附加修复：** 编译期发现 `usage_stats.rs:1061` 缺少 `.to_string()` —— 本仓新增的 `share_id` 过滤分支 `conditions.push("l.share_id = ?")` 与上游把 `conditions` 改成 `Vec<String>` 的合并产物，补齐 `.to_string()`
- **验证：** `cargo check --bins --tests`、`cargo test --lib`（1126 通过）、`cargo test --test hermes_roundtrip --test skill_sync --test import_export_sync --test mcp_commands`（25 通过）、`pnpm typecheck`、`pnpm test:unit`（262 通过）
- **注：** 合并前 stash 了本地 Gemini OAuth WIP（5 个新文件 + 多文件改动），合并完成后 `git stash pop` 恢复

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
