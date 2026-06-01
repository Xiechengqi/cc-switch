//! Share 绑定健康事件
//!
//! 当某个 share 在请求路径上发现其绑定的 provider 缺失、被禁用、
//! 或与 share.app_type 不匹配时，通过本模块向前端 emit
//! `share-needs-rebind` 事件，让 UI 提示用户去补绑。
//!
//! 这是稀有事件（运行时数据不一致才会触发），不做防抖。
//! 与 [`crate::usage_events`] 共享同样的"setup 注入 AppHandle"风格。

use std::sync::OnceLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// 前端监听的事件名
pub const EVENT_SHARE_NEEDS_REBIND: &str = "share-needs-rebind";

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareNeedsRebindPayload {
    pub share_id: String,
    pub app_type: String,
    pub reason: ShareRebindReason,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShareRebindReason {
    /// share.provider_id 在 providers 表里找不到
    ProviderMissing,
    /// share.provider_id 指向的 provider.app_type ≠ share.app_type
    AppTypeMismatch,
    /// 绑定 provider 已经被熔断且无法恢复（保留扩展位）
    ProviderUnavailable,
}

/// 在应用 setup 阶段调用一次，注入 AppHandle。
pub fn init(handle: AppHandle) {
    if APP_HANDLE.set(handle).is_err() {
        log::debug!("share_events::init 重复调用，已忽略");
    } else {
        log::info!("[share-event] AppHandle 已注入，事件推送启用");
    }
}

/// 通知前端某个 share 需要被用户重新绑定 provider。
///
/// 调用方**不**需要持有 AppHandle。AppHandle 未注入或 emit 失败都只写
/// warn 日志，绝不向上传播错误——share 路径上的健康事件不应阻塞请求。
pub fn notify_share_needs_rebind(
    share_id: impl Into<String>,
    app_type: impl Into<String>,
    reason: ShareRebindReason,
    detail: Option<String>,
) {
    let payload = ShareNeedsRebindPayload {
        share_id: share_id.into(),
        app_type: app_type.into(),
        reason,
        detail,
    };

    let Some(handle) = APP_HANDLE.get() else {
        log::debug!(
            "[share-event] AppHandle 未注入，丢弃 share-needs-rebind: share_id={} reason={:?}",
            payload.share_id,
            payload.reason
        );
        return;
    };

    if let Err(e) = handle.emit(EVENT_SHARE_NEEDS_REBIND, &payload) {
        log::warn!(
            "emit {EVENT_SHARE_NEEDS_REBIND} 失败 share_id={} reason={:?}: {e}",
            payload.share_id,
            payload.reason
        );
    }
}
