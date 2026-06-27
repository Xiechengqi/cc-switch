# no-desktop 与 Web 入口规划

## 定位

Web 入口是 cc-switch desktop 后端能力的远程管理界面。`--no-desktop` 是无窗口运行同一套后端能力的模式，主要用于服务器环境，不是独立 server 产品线。

因此，no-desktop 不应复制一整套和 desktop 不同的业务语义。两种模式应共享数据库、配置、供应商、OAuth、proxy 接管和 web 鉴权。

## 必须保持一致的行为

desktop 和 no-desktop 之间切换时必须保持：

- 相同 app config dir
- 相同 SQLite 数据库
- 相同 provider 列表
- 相同 current provider 和模型映射
- 相同 OAuth 管理状态
- 相同 Live 配置接管和恢复行为
- 相同 web password / email / api token 鉴权策略
- 相同 share/router/tunnel 状态恢复

不能因为从 desktop 切到 no-desktop，或从 no-desktop 切回 desktop，导致供应商不显示、Live 配置损坏、web 登录失效或 tunnel 注册状态丢失。

## 启动结构

`src-tauri/src/lib.rs` 只负责入口分流：

- 普通 desktop：启动 Tauri desktop
- `--no-desktop`：进入 `src-tauri/src/headless.rs`

`src-tauri/src/headless.rs` 负责 no-desktop 专属部分：

- 初始化 headless runtime
- 读取相同 app config dir
- 初始化数据库
- 初始化 headless OAuth globals
- 启动 proxy/web 入口

共享启动扩展逻辑放在：

- `src-tauri/src/local_ext/startup.rs`

该模块负责：

- 恢复 Live 接管残留
- 初始化 common config snippets
- 恢复本地路由常开
- 恢复 share/client tunnel
- 启动 router sync
- 启动 share model health
- 启动 periodic backup
- 启动 session usage sync

## Web 鉴权方向

Web 应支持 password 鉴权。首次 no-desktop 启动时，不需要 setup token；首次访问直接设置 password。

当 cc-switch 未注册 router 时，应允许仅通过 password 登录。

当注册到 router 后，允许通过以下方式登录：

- email 验证
- api token
- password

首次 setup 如果缺少 share owner email、router、client url 信息，应在 Web setup 流程中引导配置。client tunnel subdomain 可选；不填时使用 email 前缀加随机后缀生成默认值。

## 开发原则

no-desktop 的新增行为优先复用 desktop 后端服务，不要复制一套独立逻辑。

Web handler 如果需要同时支持 desktop 和 no-desktop，应通过明确的状态适配层访问 `AppState`，不要在每个 handler 内部分散判断模式。

涉及启动流程的本地扩展优先放入 `local_ext/startup.rs`。只有真正和 Tauri 窗口、托盘、deep link 强相关的逻辑才留在 `lib.rs`。

## 验证重点

每次修改 no-desktop 或 web 入口后，至少验证：

- desktop 正常启动
- `cc-switch --no-desktop` 不初始化 GTK/Tauri 窗口
- `http://localhost:15721` 能访问 Web 入口
- desktop 与 no-desktop 切换后 provider 仍存在
- Live 配置接管状态不丢失
- share/client tunnel 能恢复
- password 首次设置和后续登录正常
