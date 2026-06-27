# cc-switch 本地架构规划

## 核心结论

cc-switch 当前最优方向不是彻底 server 化，也不是完全放弃 desktop，而是保持 upstream desktop 为主体，同时把本地 share/router/market/web/no-desktop 增量隔离在清晰边界内。

Web 入口应作为 desktop 后端能力的远程管理面；`--no-desktop` 是无窗口运行同一套后端能力的附属模式。这样既能满足服务器环境部署，又能继续吸收上游 Claude、Codex、Gemini 反代能力的修复。

## 为什么不建议完全 server 化

上游最近持续修改账号反代核心路径，包括：

- Codex Responses API / Chat API 转换
- Claude / Anthropic 消息结构和 system message 行为
- OAuth token 保留、刷新和代理路径
- streaming、SSE、tool call、thinking、cache 字段
- body 解压、zstd、转发细节
- 模型映射和用量统计

这些变化直接影响账号反代稳定性。如果完全脱离上游，短期可以运行，但长期需要自己维护协议兼容和回归测试，成本很高。

## 本地能力隔离原则

本地增量应尽量集中到低冲突模块：

- `src-tauri/src/local_ext/`：本地扩展启动和跨模式复用逻辑
- `src-tauri/src/web/`：Web 管理入口和鉴权
- `src-tauri/src/tunnel/`：router 隧道、share 同步、健康数据
- `src-tauri/src/services/share.rs`：share 业务服务
- `src-tauri/src/commands/share.rs`：share 桌面命令入口

上游高频文件只保留必要挂载点，不放大本地业务逻辑：

- `src-tauri/src/lib.rs`
- `src-tauri/src/proxy/forwarder.rs`
- `src-tauri/src/proxy/handlers.rs`
- `src-tauri/src/proxy/providers/**`
- `src-tauri/src/provider.rs`
- `src-tauri/src/services/provider/**`
- `src-tauri/src/database/schema.rs`

## 当前已形成的方向

已新增 `src-tauri/src/local_ext/startup.rs`，用于集中本地启动扩展逻辑，包括：

- Live 接管残留恢复
- common config snippet 初始化
- 本地路由常开恢复
- share tunnel 恢复
- router sync 后台任务
- share model health 后台任务
- periodic backup
- session usage sync

`src-tauri/src/lib.rs` 只保留 desktop 主流程和一个本地扩展挂载点。`src-tauri/src/headless.rs` 仍负责 no-desktop 自身 runtime、数据库和 proxy 启动，但复用本地扩展启动逻辑。

## 后续开发要求

新增本地能力时，优先寻找现有扩展层入口，不要直接把业务逻辑塞进 `lib.rs` 或 proxy provider 内部。

如果必须改 proxy/provider 核心文件，需要先判断是否属于上游也可能修复的协议问题。协议问题优先跟随或兼容上游实现，本地只做最小补丁。

删除上游功能不是默认选择。若 MCP、skills、session manager 等功能不符合当前产品聚焦，优先隐藏入口或降低使用权重，避免后续合并时反复冲突。
