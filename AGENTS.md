# cc-switch 本地开发约定

## 产品方向

本 fork 继续以 upstream desktop cc-switch 为基础。Web 入口是 desktop 后端的远程管理界面，`--no-desktop` 是无窗口运行同一套后端能力的附属模式，不是独立 server 产品主线。

除非用户明确要求，不要把项目改造成完全独立的 server 架构，也不要停止吸收上游核心反代修复。

## 架构边界

本地 share、router、market、web、no-desktop 增量应尽量隔离在本地扩展层，优先使用这些路径：

- `src-tauri/src/local_ext/`
- `src-tauri/src/web/`
- `src-tauri/src/tunnel/`
- `src-tauri/src/services/share.rs`
- `src-tauri/src/commands/share.rs`

尽量避免不必要地修改上游高频冲突路径：

- `src-tauri/src/proxy/forwarder.rs`
- `src-tauri/src/proxy/handlers.rs`
- `src-tauri/src/proxy/providers/**`
- `src-tauri/src/provider.rs`
- `src-tauri/src/services/provider/**`
- `src-tauri/src/database/schema.rs`
- `src-tauri/src/lib.rs`

如果必须修改这些文件，保持改动最小，并说明原因。

## 上游合并策略

继续跟进上游 Claude、Codex、Gemini 账号反代相关修复，尤其是：

- OAuth/token 保留与刷新
- Claude/Codex/Gemini 请求和响应转换
- Responses API / Chat API 兼容
- streaming、SSE、tool call、thinking、cache 字段
- 压缩、解压、body 转发
- 模型映射、用量统计、实际模型记录

不要为“功能清理”盲目删除 MCP、skills、session manager、updater 等上游功能。若当前产品不聚焦这些功能，优先在 UI 或入口层隐藏或不使用，避免未来合并冲突扩大。

## desktop / no-desktop 兼容

desktop 和 no-desktop 必须共享配置目录、数据库、供应商状态、OAuth 状态、proxy 接管行为和 web 鉴权行为。

两种模式之间反复切换不能导致供应商消失、Live 配置损坏、web 登录失效或 proxy 接管状态错乱。

no-desktop 应复用共享启动逻辑。本地启动扩展逻辑优先放在：

- `src-tauri/src/local_ext/startup.rs`

## 验证要求

完成代码改动后，优先运行：

- `cargo fmt --check`
- `cargo check --offline`
- `npm run typecheck`
- `cargo test --offline --lib`

如果存在既有 warning，需要单独说明；不能隐藏失败检查。

## 详细背景

更多规划和取舍理由见：

- `docs/local-architecture-plan.md`
- `docs/upstream-merge-policy.md`
- `docs/no-desktop-web-plan.md`
