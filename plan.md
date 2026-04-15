# Token 分享功能实现总结

基于 portr 内网穿透的 Token 分享机制，允许用户将本地代理端口（15721）以带配额限制的方式暴露给第三方使用。portr 服务端保持不变（Go），客户端用 Rust 重写并内嵌到 cc-switch。

## 整体架构

```
第三方用户                  portr 公网                     cc-switch (本机)
┌────────────┐   HTTPS    ┌─────────────┐   SSH 反向     ┌──────────────┐
│  API 调用  │ ─────────> │ portr server│ <───────────── │ 内嵌 tunnel  │
│ + X-Share- │            │  (Go, 不变) │                │ + share_guard│
│   Token    │            └─────────────┘                │ + 本地代理   │
└────────────┘                                            │   15721      │
                                                          └──────────────┘
```

## Phase 1 — Tunnel 模块（Rust portr client）

`src-tauri/src/tunnel/`：

| 文件 | 作用 |
| --- | --- |
| `error.rs` | `TunnelError` 枚举 |
| `config.rs` | `TunnelConfig`、`TunnelRequest`、`TunnelInfo`、`TunnelType` |
| `connection.rs` | `POST /api/v1/connections/` 创建 connection |
| `forward.rs` | `io::copy_bidirectional` 双向转发 |
| `ssh.rs` | `SshTunnel` — russh client、认证、`tcpip_forward`、accept loop、reconnect |
| `health.rs` | 3s 间隔 HTTP 健康检查（`X-Portr-Ping-Request`）+ 10 次连续失败触发重连 |
| `mod.rs` | `TunnelManager` — `HashMap<share_id, TunnelHandle>`，管理所有隧道生命周期 |

协议要点：
- SSH 认证：`user = "{connection_id}:{secret_key}"`，空密码
- 远程端口：在 `20000–30000` 内随机尝试 10 次
- 内部保活：russh `keepalive_interval = 15s`
- 服务端密钥：`check_server_key` 固定返回 `Ok(true)`（匹配 portr 默认）

依赖：`russh = "0.46"`、`russh-keys = "0.46"`、`rand = "0.8"`。

> ⚠️ `tunnel/ssh.rs` 的导入使用了 `russh::keys::key::PublicKey` 与原生 async trait，这对应 russh ≥ 0.52 的 API。实际编译前需要：要么将 Cargo.toml 升到 `russh = "0.52"` 并移除 `russh-keys`，要么回到 0.46 并加 `#[async_trait::async_trait]` + 用 `russh_keys::key::PublicKey`。本地环境缺 `libglib2.0-dev` 无法 `cargo check`，此决策留待开发机上执行。

## Phase 2 — Share 功能

### 数据层

- **Schema v9**（`database/schema.rs`）：新增 `shares` 表
  - 字段：`id, name, share_token, app_type, provider_id, api_key, settings_config, token_limit, tokens_used, requests_count, expires_at, subdomain, tunnel_url, status, created_at, last_used_at`
  - 索引：`idx_shares_token`
  - `SCHEMA_VERSION` 从 8 升到 9，`migrate_v8_to_v9` 已注册
- **DAO**（`database/dao/shares.rs`）：`ShareRecord` + `create/get_by_id/get_by_token/list/update_status/update_tunnel/increment_usage/delete/expire`
- **Service**（`services/share.rs`）：`ShareService` 封装业务规则
  - `validate_token`：检查 `status == "active"` 且未过期、未耗尽，自动更新为 `expired` / `exhausted`
  - `record_usage`：原子累加 `tokens_used`，达到 `token_limit` 自动置为 `exhausted`
  - `generate_token`：32 字符 nanoid 风格

### 代理集成（P1 关键）

- **`proxy/share_guard.rs`**：`check_share_token(db, headers)` 返回 `ShareGuardResult` 枚举；`record_share_usage(db, share_id, input, output)` 回写用量。
- **`proxy/handler_context.rs`**：
  - `RequestContext` 新增 `share_id: Option<String>`
  - `RequestContext::new` 在选完 provider 后调用 `try_apply_share`：
    - 无 `X-Share-Token` → 正常本地代理路径
    - token 有效 → 若 `share.provider_id` 已绑定，直接从 DB 加载目标 provider 并覆盖 `ctx.provider/providers`；未绑定则仅把故障转移链收敛到单个 provider，避免泄露其他供应商
    - token 无效/过期/耗尽 → 返回 `ProxyError::AuthError`（HTTP 401）
- **`proxy/response_processor.rs`**：`spawn_log_usage` 在正常写用量日志之后，若 `ctx.share_id.is_some()`，再调用 `share_guard::record_share_usage` 回写分享用量（自动触发 exhausted 保护）。

### Tauri Commands（`commands/share.rs`）

| 命令 | 作用 |
| --- | --- |
| `create_share` | 创建分享记录（`name/app_type/provider_id?/api_key/settings_config?/token_limit/expires_in_secs`） |
| `delete_share` | **async**：先停止对应隧道再删除 DB 行（修复 P2 泄漏） |
| `pause_share` / `resume_share` | 切换 `status` |
| `list_shares` / `get_share_detail` | 查询 |
| `start_share_tunnel` / `stop_share_tunnel` / `get_tunnel_status` | 隧道生命周期 |
| `get_share_connect_info` | 返回 `tunnel_url + share_token + subdomain` |
| `configure_tunnel` | 持久化到 `AppSettings` 并同步更新内存 `TunnelManager`（修复 P2 重启失效） |

### 应用状态与设置

- **`store.rs`**：`AppState` 新增 `tunnel_manager: Arc<RwLock<TunnelManager>>`
- **`settings.rs`**：`AppSettings` 新增 `portr_server_url / portr_ssh_url / portr_tunnel_url / portr_secret_key / portr_use_localhost`
- **`lib.rs`**：
  - 声明 `mod tunnel`
  - 在 `AppState::new(db)` 之后读取 `get_settings()`，若四个核心 portr 字段齐备则 `block_on` 将配置灌入 `TunnelManager`（修复 P2 配置持久化）
  - `invoke_handler!` 注册全部 11 个 share 命令

### 前端 API

- **`src/lib/api/share.ts`**：与后端 commands 一一对应的 `invoke` 封装，含 `ShareRecord / CreateShareParams / TunnelInfo / TunnelConfig / ConnectInfo` TS 类型

## 已修复的 Codex Review 发现

| 级别 | 问题 | 修复位置 |
| --- | --- | --- |
| P1 | share token 未接入请求处理路径 | `handler_context.rs::try_apply_share` + `response_processor.rs::spawn_log_usage` |
| P2 | 删除分享未停止活动隧道 | `commands/share.rs::delete_share`（改 async，先 stop 后 delete） |
| P2 | 隧道配置仅存内存、重启失效 | `commands/share.rs::configure_tunnel` 写入 `AppSettings` + `lib.rs` 启动时恢复 |

## 遗留事项（非本次实现覆盖）

1. **编译验证**：本环境缺 `libglib2.0-dev` 等 GTK/WebKit 开发包，`cargo check` 无法运行，需要在具备 Tauri Linux 构建依赖的开发机上执行。
2. **russh 版本对齐**：见 Phase 1 警告。`tunnel/ssh.rs` 与当前 Cargo.toml 不完全匹配，需要二选一调整。
3. **前端 UI**：`src/components/share/` 尚未创建，只完成了 API 层（`src/lib/api/share.ts`）。
4. **设置页表单**：Settings 页面尚未暴露 5 个 `portr_*` 字段的编辑入口。
5. **转换链路下的用量回写**：`proxy/handlers.rs::handle_claude_transform` 和部分流式 usage collector 使用的是独立的 `log_usage`（不经 `spawn_log_usage`），如果分享请求走到 OpenRouter 转换路径，这些位置还需要补充 `record_share_usage` 调用；当前版本对透传模式的 Claude/Codex/Gemini 已足够。
