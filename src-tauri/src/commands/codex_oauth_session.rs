//! Codex Session Import Tauri Commands
//!
//! Surface two commands to the frontend:
//!
//! - `preview_codex_session_parse`: pure parse, no I/O. UI uses this to show a
//!   "we detected N sessions" preview as the user types/pastes, without
//!   committing anything to the managed account pool. Token material is
//!   redacted from the preview payload — only identity metadata returns.
//! - `import_codex_sessions`: parse + write into the `CodexOAuthManager`.
//!   Mirrors sub2api's row-by-row result shape (`total / created / updated /
//!   skipped / failed`) so the UI can render the same per-row table.
//!
//! Manager wiring lives in `crate::proxy::providers::codex_oauth_auth`; the
//! per-format parsers live in
//! `crate::proxy::providers::codex_oauth_session`.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::codex_oauth::CodexOAuthState;
use crate::proxy::providers::codex_oauth_auth::{CodexImportAction, CodexImportOutcome};
use crate::proxy::providers::codex_oauth_session::{
    decrypt_cc_switch_envelope, parse_many, redact_session, render, render_cc_switch_envelope,
    render_encrypted_cc_switch_envelope, render_many_jsonl, render_sub2api_batch, sniff_format,
    suggest_batch_filename, suggest_envelope_filename, suggest_single_filename,
    CanonicalCodexSession, CodexEnvelopeCryptoError, CodexExportFormat, CodexSessionParseError,
    CodexSessionSource,
};

// ─────────────────────────── machine id ────────────────────────────────────

/// Persistent random identifier for this cc-switch installation, used as the
/// `exported_by_machine_id` field on envelope exports so an import on a
/// different machine can warn the user "this came from somewhere else".
///
/// Stored at `<app_config_dir>/codex-session-machine-id.txt` as raw text.
/// Generated lazily on first read; never modified afterward. Losing the file
/// (manual delete, fresh install) just makes the next export look "new" —
/// no functional impact beyond the warning suppression.
fn machine_id_path() -> std::path::PathBuf {
    crate::config::get_app_config_dir().join("codex-session-machine-id.txt")
}

pub fn current_machine_id() -> String {
    let path = machine_id_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // Random 16-byte token, lowercase hex. Not a security boundary — just an
    // identity tag that's unlikely to collide.
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &id);
    id
}

#[tauri::command]
pub fn get_codex_session_machine_id() -> String {
    current_machine_id()
}

/// Hard cap on a single import blob, matching the design doc: 1 MiB. Avoids
/// pasted log files producing parser pathological behavior or DoSing the IPC.
const MAX_IMPORT_BLOB_BYTES: usize = 1 * 1024 * 1024;

// ─────────────────────────── preview ────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionPreviewResult {
    pub sniffed_format: CodexSessionSource,
    pub total: usize,
    pub items: Vec<CodexSessionPreviewItem>,
    /// Identifier the cc-switch envelope was exported from, when present.
    /// `None` for non-envelope inputs and for envelopes that omitted the field.
    /// The frontend compares this against `get_codex_session_machine_id` to
    /// decide whether to surface a "this backup came from another machine"
    /// confirmation step.
    pub envelope_source_machine_id: Option<String>,
    /// True when the input is an `encrypted: true` envelope — the UI uses
    /// this to surface the password field even before parsing succeeds.
    pub envelope_encrypted: bool,
}

/// Per-row preview entry. Token material is intentionally NOT serialized —
/// the UI only needs identity + a flag to drive the "has refresh / no refresh"
/// warning chip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionPreviewItem {
    pub index: usize,
    pub source: CodexSessionSource,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub organization_id: Option<String>,
    pub exp: Option<i64>,
    pub has_refresh_token: bool,
    pub has_id_token: bool,
    pub is_expired: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl CodexSessionPreviewItem {
    fn from_session(index: usize, session: &CanonicalCodexSession, now_secs: i64) -> Self {
        let mut warnings = Vec::new();
        if session.refresh_token.is_none() {
            warnings.push("未包含 refresh_token，导入后无法自动续期".to_string());
        }
        if session.account_id.is_none() {
            warnings.push("无法从 token 中提取 chatgpt_account_id".to_string());
        }
        if session.exp.is_none() {
            warnings.push("无法解析 access_token 过期时间".to_string());
        }
        Self {
            index,
            source: session.source,
            account_id: session.account_id.clone(),
            user_id: session.user_id.clone(),
            email: session.email.clone(),
            plan_type: session.plan_type.clone(),
            organization_id: session.organization_id.clone(),
            exp: session.exp,
            has_refresh_token: session.refresh_token.is_some(),
            has_id_token: session.id_token.is_some(),
            is_expired: session.is_expired(now_secs),
            error: None,
            warnings,
        }
    }

    fn from_error(index: usize, err: &CodexSessionParseError) -> Self {
        Self {
            index,
            source: CodexSessionSource::Unknown,
            account_id: None,
            user_id: None,
            email: None,
            plan_type: None,
            organization_id: None,
            exp: None,
            has_refresh_token: false,
            has_id_token: false,
            is_expired: false,
            error: Some(err.to_string()),
            warnings: Vec::new(),
        }
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn preview_codex_session_parse(text: String) -> Result<CodexSessionPreviewResult, String> {
    if text.len() > MAX_IMPORT_BLOB_BYTES {
        return Err(format!(
            "输入过长 ({} 字节)，单次粘贴不要超过 {} KiB",
            text.len(),
            MAX_IMPORT_BLOB_BYTES / 1024
        ));
    }
    let sniffed_format = sniff_format(&text);
    let now_secs = chrono::Utc::now().timestamp();
    let (envelope_source_machine_id, envelope_encrypted) = inspect_envelope_header(&text);
    let parsed = parse_many(&text);
    let total = parsed.len();
    let items = parsed
        .iter()
        .enumerate()
        .map(|(i, result)| match result {
            Ok(session) => CodexSessionPreviewItem::from_session(i + 1, session, now_secs),
            Err(err) => CodexSessionPreviewItem::from_error(i + 1, err),
        })
        .collect();
    Ok(CodexSessionPreviewResult {
        sniffed_format,
        total,
        items,
        envelope_source_machine_id,
        envelope_encrypted,
    })
}

/// Peek at a JSON envelope's plaintext header without parsing the (possibly
/// encrypted) payload. Returns `(machine_id, encrypted)` — both `None`/false
/// when the input isn't a cc-switch envelope at all.
fn inspect_envelope_header(text: &str) -> (Option<String>, bool) {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') {
        return (None, false);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return (None, false);
    };
    let is_envelope = value
        .get("format")
        .and_then(serde_json::Value::as_str)
        .map(|s| s == "cc-switch-codex-export")
        .unwrap_or(false);
    if !is_envelope {
        return (None, false);
    }
    let mid = value
        .get("exported_by_machine_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let enc = value
        .get("encrypted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (mid, enc)
}

// ─────────────────────────── import ─────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionImportRequest {
    pub content: String,
    #[serde(default = "default_true")]
    pub update_existing: bool,
    /// When true (default), refuse to import a session whose access_token is
    /// already past `exp` even if it carries a refresh_token. Set false if
    /// the user explicitly wants to import a stale session and let the next
    /// refresh roundtrip recover.
    #[serde(default = "default_true")]
    pub reject_expired: bool,
    /// When true, immediately call `get_valid_token_for_account` after each
    /// successful import to validate the refresh_token is live. Per-row
    /// failure becomes a downgraded `Updated`/`Created` → `Failed` outcome
    /// with the refresh error surfaced; the account is left in place so the
    /// user can retry. Off by default to keep import latency predictable for
    /// large batches.
    #[serde(default)]
    pub verify_refresh: bool,
    /// Password for decrypting an encrypted cc-switch envelope. Required when
    /// the input is a `cc-switch-codex-export` envelope with `encrypted: true`;
    /// silently ignored otherwise. Empty string is treated as "no password".
    #[serde(default)]
    pub password: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionImportResult {
    pub total: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
    pub items: Vec<CodexSessionImportItem>,
    pub warnings: Vec<CodexSessionImportMessage>,
    pub errors: Vec<CodexSessionImportMessage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionImportItem {
    pub index: usize,
    pub action: CodexSessionImportAction,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexSessionImportAction {
    Created,
    Updated,
    Skipped,
    Failed,
}

impl From<CodexImportAction> for CodexSessionImportAction {
    fn from(value: CodexImportAction) -> Self {
        match value {
            CodexImportAction::Created => CodexSessionImportAction::Created,
            CodexImportAction::Updated => CodexSessionImportAction::Updated,
            CodexImportAction::Skipped => CodexSessionImportAction::Skipped,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionImportMessage {
    pub index: usize,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub message: String,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn import_codex_sessions(
    request: CodexSessionImportRequest,
    state: State<'_, CodexOAuthState>,
) -> Result<CodexSessionImportResult, String> {
    if request.content.len() > MAX_IMPORT_BLOB_BYTES {
        return Err(format!(
            "输入过长 ({} 字节)，单次粘贴不要超过 {} KiB",
            request.content.len(),
            MAX_IMPORT_BLOB_BYTES / 1024
        ));
    }

    // Encrypted-envelope detection: when the blob parses as JSON with
    // `encrypted: true`, decrypt it before handing to parse_many so the rest
    // of the import path doesn't care about the envelope at all. Plaintext
    // input falls through unchanged.
    let content_for_parse = decrypt_envelope_if_needed(
        &request.content,
        request
            .password
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .map_err(|e| e.to_string())?;

    let parsed = parse_many(&content_for_parse);
    let total = parsed.len();
    let now_secs = chrono::Utc::now().timestamp();

    let mut result = CodexSessionImportResult {
        total,
        created: 0,
        updated: 0,
        skipped: 0,
        failed: 0,
        items: Vec::with_capacity(total),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    // Within-batch dedup: when the same blob carries the same session twice
    // (sub2api batch with rotation, or accidental double-paste), only the
    // first occurrence runs the manager write; subsequent rows return a
    // structured "duplicate" entry without touching disk.
    let mut seen_keys: std::collections::HashMap<String, usize> = Default::default();

    // Hold the write lock once for the whole batch — keeps imports atomic from
    // the perspective of other readers (no half-written batch visible) and
    // avoids per-row save_to_disk thrash.
    let manager = state.0.write().await;

    for (i, parse_result) in parsed.into_iter().enumerate() {
        let index = i + 1;
        match parse_result {
            Err(err) => {
                result.failed += 1;
                let message = err.to_string();
                result.items.push(CodexSessionImportItem {
                    index,
                    action: CodexSessionImportAction::Failed,
                    account_id: None,
                    email: None,
                    message: Some(message.clone()),
                });
                result.errors.push(CodexSessionImportMessage {
                    index,
                    account_id: None,
                    email: None,
                    message,
                });
            }
            Ok(session) => {
                let identity_keys = session.identity_keys();
                if let Some(first_index) =
                    identity_keys.iter().find_map(|k| seen_keys.get(k).copied())
                {
                    let message = format!("与第 {first_index} 条导入项重复，已跳过");
                    result.skipped += 1;
                    result.items.push(CodexSessionImportItem {
                        index,
                        action: CodexSessionImportAction::Skipped,
                        account_id: session.account_id.clone(),
                        email: session.email.clone(),
                        message: Some(message.clone()),
                    });
                    result.warnings.push(CodexSessionImportMessage {
                        index,
                        account_id: session.account_id.clone(),
                        email: session.email.clone(),
                        message,
                    });
                    continue;
                }
                for key in &identity_keys {
                    seen_keys.entry(key.clone()).or_insert(index);
                }

                if request.reject_expired && session.is_expired(now_secs) {
                    let message = format!(
                        "access_token 已过期 (exp={})，拒绝导入",
                        session.exp.unwrap_or(0)
                    );
                    result.failed += 1;
                    result.items.push(CodexSessionImportItem {
                        index,
                        action: CodexSessionImportAction::Failed,
                        account_id: session.account_id.clone(),
                        email: session.email.clone(),
                        message: Some(message.clone()),
                    });
                    result.errors.push(CodexSessionImportMessage {
                        index,
                        account_id: session.account_id.clone(),
                        email: session.email.clone(),
                        message,
                    });
                    continue;
                }

                match manager
                    .import_canonical_session_without_persist(&session, request.update_existing)
                    .await
                {
                    Ok(CodexImportOutcome { account, action }) => {
                        match action {
                            CodexImportAction::Created => result.created += 1,
                            CodexImportAction::Updated => result.updated += 1,
                            CodexImportAction::Skipped => {
                                result.skipped += 1;
                                let message = "账号已存在，未启用覆盖".to_string();
                                result.warnings.push(CodexSessionImportMessage {
                                    index,
                                    account_id: Some(account.id.clone()),
                                    email: account.email.clone(),
                                    message: message.clone(),
                                });
                                result.items.push(CodexSessionImportItem {
                                    index,
                                    action: CodexSessionImportAction::Skipped,
                                    account_id: Some(account.id),
                                    email: account.email,
                                    message: Some(message),
                                });
                                continue;
                            }
                        }

                        // Optional verify_refresh: validate refresh_token immediately by
                        // forcing a refresh-path read. We invalidate the cached access_token
                        // first so get_valid_token_for_account is guaranteed to exercise the
                        // refresh endpoint rather than serving the value we just seeded.
                        let mut verify_message: Option<String> = None;
                        let mut downgraded_to_failed = false;
                        if request.verify_refresh {
                            manager.invalidate_cached_token(&account.id).await;
                            if let Err(verify_err) =
                                manager.get_valid_token_for_account(&account.id).await
                            {
                                let msg = format!("导入后续期验证失败: {verify_err}");
                                downgraded_to_failed = true;
                                match action {
                                    CodexImportAction::Created => {
                                        result.created = result.created.saturating_sub(1)
                                    }
                                    CodexImportAction::Updated => {
                                        result.updated = result.updated.saturating_sub(1)
                                    }
                                    _ => {}
                                }
                                result.failed += 1;
                                result.errors.push(CodexSessionImportMessage {
                                    index,
                                    account_id: Some(account.id.clone()),
                                    email: account.email.clone(),
                                    message: msg.clone(),
                                });
                                verify_message = Some(msg);
                            }
                        }

                        let resolved_action = if downgraded_to_failed {
                            CodexSessionImportAction::Failed
                        } else {
                            action.into()
                        };
                        result.items.push(CodexSessionImportItem {
                            index,
                            action: resolved_action,
                            account_id: Some(account.id),
                            email: account.email,
                            message: verify_message,
                        });
                    }
                    Err(err) => {
                        result.failed += 1;
                        let message = err.to_string();
                        result.items.push(CodexSessionImportItem {
                            index,
                            action: CodexSessionImportAction::Failed,
                            account_id: session.account_id.clone(),
                            email: session.email.clone(),
                            message: Some(message.clone()),
                        });
                        result.errors.push(CodexSessionImportMessage {
                            index,
                            account_id: session.account_id,
                            email: session.email,
                            message,
                        });
                    }
                }
            }
        }
    }

    // Single fsync for the whole batch. import_canonical_session_without_persist
    // mutates the in-memory store immediately, so even if persistence fails
    // here the imports are functional for the rest of the process lifetime —
    // we still surface the error to the user since restarting would lose them.
    if result.created > 0 || result.updated > 0 {
        if let Err(err) = manager.persist_imports().await {
            result.errors.push(CodexSessionImportMessage {
                index: 0,
                account_id: None,
                email: None,
                message: format!("批量导入完成但持久化失败: {err}"),
            });
        }
    }

    Ok(result)
}

// ─────────────────────────── export ─────────────────────────────────────────

/// Target shape for a single export. `CcSwitchEnvelope` is a batch-only mode
/// (it wraps every selected account regardless of count) so it lives on a
/// separate axis from per-item formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexExportFormatTag {
    CodexCli,
    Cpa,
    Sub2api,
    RawJwt,
    CcSwitchEnvelope,
}

impl CodexExportFormatTag {
    fn per_item(self) -> Option<CodexExportFormat> {
        match self {
            CodexExportFormatTag::CodexCli => Some(CodexExportFormat::CodexCli),
            CodexExportFormatTag::Cpa => Some(CodexExportFormat::Cpa),
            CodexExportFormatTag::Sub2api => Some(CodexExportFormat::Sub2api),
            CodexExportFormatTag::RawJwt => Some(CodexExportFormat::RawJwt),
            CodexExportFormatTag::CcSwitchEnvelope => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionExportRequest {
    /// When empty, exports every managed account.
    #[serde(default)]
    pub account_ids: Vec<String>,
    pub format: CodexExportFormatTag,
    /// Whether to call `get_valid_token_for_account` before serializing so
    /// downstream consumers always receive a freshly-refreshed access_token.
    /// Default true: stale exports defeat the entire point in a deployment
    /// without a Codex CLI to refresh in place.
    #[serde(default = "default_true")]
    pub refresh_first: bool,
    /// When true, replace token material with deterministic SHA-256 markers.
    /// Useful for sharing debug payloads. Disables the multi-instance warning
    /// (a redacted file can't be used anywhere).
    #[serde(default)]
    pub redact: bool,
    /// Optional machine identifier embedded into cc-switch envelope exports so
    /// receivers can detect "the file came from a different machine" later.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// After a successful export, mark each successfully-exported account as
    /// "handed off" so cc-switch stops auto-refreshing it. Use when the user
    /// is transferring ownership to a downstream consumer permanently and
    /// doesn't want rotation collisions. Default false; the multi-instance
    /// warning still flags the risk for non-handoff exports.
    #[serde(default)]
    pub mark_handoff: bool,
    /// Optional password for the cc-switch envelope format. Only honored when
    /// `format == CcSwitchEnvelope` — every other format ignores it and the
    /// command rejects with `format does not support encryption`. Empty string
    /// is treated as "no password" (same as omitting the field).
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionExportResult {
    pub format: CodexExportFormatTag,
    pub suggested_filename: String,
    pub payload: String,
    pub redacted: bool,
    pub account_count: usize,
    pub warnings: Vec<String>,
    pub items: Vec<CodexSessionExportItem>,
    /// Ready-to-paste curl command when `format == Sub2api`. Targets the
    /// admin API endpoint that ingests this exact payload. Populated only
    /// for sub2api exports; `None` for every other format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curl_command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionExportItem {
    pub account_id: String,
    pub email: Option<String>,
    pub status: CodexSessionExportStatus,
    pub exp: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexSessionExportStatus {
    Ok,
    RefreshFailed,
    NotFound,
}

#[tauri::command(rename_all = "camelCase")]
pub async fn export_codex_sessions(
    request: CodexSessionExportRequest,
    state: State<'_, CodexOAuthState>,
) -> Result<CodexSessionExportResult, String> {
    let manager = state.0.read().await;

    // Resolve which accounts to export. Empty input expands to "all", matching
    // the affordance of select-all in the UI.
    let target_ids: Vec<String> = if request.account_ids.is_empty() {
        manager
            .list_accounts()
            .await
            .into_iter()
            .map(|account| account.id)
            .collect()
    } else {
        // Preserve caller order so the export file lays out in the order the
        // user selected — useful when they're shaping a deliberate batch.
        request.account_ids.clone()
    };

    let mut items = Vec::with_capacity(target_ids.len());
    let mut sessions: Vec<CanonicalCodexSession> = Vec::with_capacity(target_ids.len());

    for id in &target_ids {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        match manager.export_account(trimmed, request.refresh_first).await {
            Ok(session) => {
                let session = if request.redact {
                    redact_session(&session)
                } else {
                    session
                };
                items.push(CodexSessionExportItem {
                    account_id: trimmed.to_string(),
                    email: session.email.clone(),
                    status: CodexSessionExportStatus::Ok,
                    exp: session.exp,
                    message: None,
                });
                sessions.push(session);
            }
            Err(err) => {
                let status = match &err {
                    crate::proxy::providers::codex_oauth_auth::CodexOAuthError::AccountNotFound(
                        _,
                    ) => CodexSessionExportStatus::NotFound,
                    _ => CodexSessionExportStatus::RefreshFailed,
                };
                items.push(CodexSessionExportItem {
                    account_id: trimmed.to_string(),
                    email: None,
                    status,
                    exp: None,
                    message: Some(err.to_string()),
                });
            }
        }
    }

    // mark_handoff: after we've collected sessions, atomically flip each
    // successfully-exported account into handed-off mode while we still hold
    // the manager. Runs only on Ok rows so failures don't poison the user's
    // managed pool.
    let mut handoff_failures: Vec<(String, String)> = Vec::new();
    if request.mark_handoff {
        for item in &items {
            if item.status == CodexSessionExportStatus::Ok {
                if let Err(err) = manager.mark_account_handoff(&item.account_id).await {
                    handoff_failures.push((item.account_id.clone(), err.to_string()));
                }
            }
        }
    }

    // Free the manager lock before rendering — pure CPU work below shouldn't
    // block account list / refresh callers any longer than necessary.
    drop(manager);

    let mut warnings: Vec<String> = Vec::new();
    // Multi-instance hazard warning: any exported payload that retains a usable
    // refresh_token enables a second consumer to rotate it, which silently
    // poisons cc-switch's stored copy. Only mute the warning when the file is
    // either redacted (unusable) or RawJwt (no refresh_token to rotate).
    let format_can_rotate = matches!(
        request.format,
        CodexExportFormatTag::CodexCli
            | CodexExportFormatTag::Cpa
            | CodexExportFormatTag::Sub2api
            | CodexExportFormatTag::CcSwitchEnvelope
    );
    if !request.redact && format_can_rotate && !sessions.is_empty() {
        warnings.push(
            "导出的 session 含可用 refresh_token，请确保只有一个消费方会续期；\
             多端同时自动续期会因 refresh_token 轮换互相作废。"
                .to_string(),
        );
    }
    if sessions.is_empty() {
        warnings.push("没有可导出的账号（账号为空或全部解析失败）".to_string());
    }
    if request.mark_handoff && !sessions.is_empty() {
        let handed_off_count = sessions.len() - handoff_failures.len();
        if handed_off_count > 0 {
            warnings.push(format!(
                "已将 {handed_off_count} 个账号标记为交接态，本地不再续期；如需收回请在前端 restore",
            ));
        }
        for (account_id, msg) in &handoff_failures {
            warnings.push(format!("账号 {account_id} 标记交接失败: {msg}"));
        }
    }

    let account_count = sessions.len();

    // Choose between single-object and batched layouts. The shape depends on
    // both the format and the count — sub2api accepts a `contents:[]` batch
    // natively, A/B fall back to JSONL when there's more than one item, raw
    // JWT becomes newline-delimited text, and the cc-switch envelope is the
    // only format that's always a single envelope object.
    let password = request
        .password
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if password.is_some() && !matches!(request.format, CodexExportFormatTag::CcSwitchEnvelope) {
        return Err(format!(
            "password is only valid with CcSwitchEnvelope; format {:?} does not support encryption",
            request.format
        ));
    }
    if request.redact && password.is_some() {
        return Err("redact + password is contradictory: a redacted file is unusable to receivers; encrypt OR redact, not both".to_string());
    }
    // Resolve machine_id: caller-supplied wins (lets a future "anonymize"
    // toggle hand in a synthetic id), otherwise default to this installation's
    // persistent id so receivers on other machines can detect cross-host
    // backups on import.
    let resolved_machine_id = request
        .machine_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(current_machine_id()));
    let (payload, suggested_filename) = render_payload(
        &sessions,
        request.format,
        resolved_machine_id.as_deref(),
        password,
    )
    .map_err(|e| e.to_string())?;

    Ok(CodexSessionExportResult {
        format: request.format,
        suggested_filename,
        payload: payload.clone(),
        redacted: request.redact,
        account_count,
        warnings,
        items,
        curl_command: if matches!(request.format, CodexExportFormatTag::Sub2api)
            && !request.redact
            && account_count > 0
        {
            Some(build_sub2api_curl(&payload))
        } else {
            None
        },
    })
}

/// Render a copy-pasteable `curl` invocation that POSTs the sub2api admin
/// import request body to a placeholder endpoint. The placeholder URL and
/// bearer token are explicit so the user notices they must replace them.
///
/// We deliberately don't try to infer the real endpoint — sub2api is
/// self-hosted by definition, the user knows their own URL, and writing it
/// here would be a fingerprint baked into every cc-switch install.
fn build_sub2api_curl(body: &str) -> String {
    // Use single-quoted heredoc-style for the body so the shell doesn't
    // re-interpret JSON. The placeholders are uppercase + bracketed so a
    // copy-paste-and-run without edits fails loudly rather than hitting the
    // wrong server.
    format!(
        "curl -X POST 'https://<SUB2API-HOST>/admin/accounts/import-codex-session' \\\n  \
         -H 'Authorization: Bearer <ADMIN-TOKEN>' \\\n  \
         -H 'Content-Type: application/json' \\\n  \
         --data-binary @- <<'CCSWITCH_PAYLOAD_EOF'\n{body}\nCCSWITCH_PAYLOAD_EOF"
    )
}

fn render_payload(
    sessions: &[CanonicalCodexSession],
    format: CodexExportFormatTag,
    machine_id: Option<&str>,
    password: Option<&str>,
) -> Result<(String, String), CodexEnvelopeCryptoError> {
    let count = sessions.len();
    Ok(match format {
        CodexExportFormatTag::CcSwitchEnvelope => {
            let ts = chrono::Utc::now().timestamp();
            let envelope = match password {
                Some(pw) => render_encrypted_cc_switch_envelope(sessions, ts, machine_id, pw)?,
                None => render_cc_switch_envelope(sessions, ts, machine_id),
            };
            (
                serde_json::to_string_pretty(&envelope).unwrap_or_default(),
                suggest_envelope_filename(ts),
            )
        }
        CodexExportFormatTag::Sub2api => {
            if count <= 1 {
                let value = sessions
                    .first()
                    .map(|s| render(s, CodexExportFormat::Sub2api))
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let payload = serde_json::to_string_pretty(&value).unwrap_or_default();
                let filename = sessions
                    .first()
                    .map(|s| suggest_single_filename(s, CodexExportFormat::Sub2api))
                    .unwrap_or_else(|| "codex-session-import.json".to_string());
                (payload, filename)
            } else {
                let value = render_sub2api_batch(sessions);
                (
                    serde_json::to_string_pretty(&value).unwrap_or_default(),
                    suggest_batch_filename(CodexExportFormat::Sub2api, count),
                )
            }
        }
        CodexExportFormatTag::RawJwt => {
            if count <= 1 {
                let payload = sessions
                    .first()
                    .map(|s| s.access_token.clone())
                    .unwrap_or_default();
                let filename = sessions
                    .first()
                    .map(|s| suggest_single_filename(s, CodexExportFormat::RawJwt))
                    .unwrap_or_else(|| "codex-access-token.jwt".to_string());
                (payload, filename)
            } else {
                let mut payload = String::new();
                for s in sessions {
                    payload.push_str(&s.access_token);
                    payload.push('\n');
                }
                (
                    payload,
                    suggest_batch_filename(CodexExportFormat::RawJwt, count),
                )
            }
        }
        per_item @ (CodexExportFormatTag::CodexCli | CodexExportFormatTag::Cpa) => {
            let inner = per_item.per_item().expect("per-item format");
            if count <= 1 {
                let value = sessions
                    .first()
                    .map(|s| render(s, inner))
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let payload = serde_json::to_string_pretty(&value).unwrap_or_default();
                let filename = sessions
                    .first()
                    .map(|s| suggest_single_filename(s, inner))
                    .unwrap_or_else(|| match inner {
                        CodexExportFormat::CodexCli => "auth.json".to_string(),
                        _ => "codex.json".to_string(),
                    });
                (payload, filename)
            } else {
                (
                    render_many_jsonl(sessions, inner),
                    suggest_batch_filename(inner, count),
                )
            }
        }
    })
}

// ─────────────────────────── handoff / restore ──────────────────────────────

/// Detect an encrypted cc-switch envelope at the top of `content`. When found
/// and `password` is supplied, decrypt and re-emit a plaintext envelope so
/// `parse_many` can handle it without any envelope/crypto awareness.
///
/// Non-envelope inputs (CodexCli JSON, CPA JSON, bare JWT, JSONL, etc.) are
/// returned unchanged regardless of `password`.
fn decrypt_envelope_if_needed(
    content: &str,
    password: Option<&str>,
) -> Result<String, CodexEnvelopeCryptoError> {
    let trimmed = content.trim();
    if !trimmed.starts_with('{') {
        return Ok(content.to_string());
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(content.to_string());
    };
    let is_envelope = value
        .get("format")
        .and_then(serde_json::Value::as_str)
        .map(|s| s == "cc-switch-codex-export")
        .unwrap_or(false);
    if !is_envelope {
        return Ok(content.to_string());
    }
    let is_encrypted = value
        .get("encrypted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !is_encrypted {
        return Ok(content.to_string());
    }
    let sessions = decrypt_cc_switch_envelope(&value, password)?;
    // Re-emit as a plaintext envelope so the canonical parse path picks it up.
    let plain = render_cc_switch_envelope(
        &sessions,
        value
            .get("exported_at")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp()),
        value
            .get("exported_by_machine_id")
            .and_then(serde_json::Value::as_str),
    );
    Ok(serde_json::to_string(&plain)
        .map_err(|e| CodexEnvelopeCryptoError::Malformed(e.to_string()))?)
}

/// Mark a managed Codex OAuth account as handed off to a downstream consumer.
/// While set, the manager refuses to refresh that account's access_token,
/// preventing concurrent rotation with the consumer. Idempotent — repeated
/// calls on an already-handed-off account succeed silently.
#[tauri::command(rename_all = "camelCase")]
pub async fn mark_codex_account_handoff(
    account_id: String,
    state: State<'_, CodexOAuthState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .mark_account_handoff(&account_id)
        .await
        .map_err(|e| e.to_string())
}

/// Reverse `mark_codex_account_handoff`. Subsequent refresh attempts will
/// hit the OAuth endpoint again; if the downstream consumer rotated the
/// refresh_token in the meantime, the next refresh will fail with
/// `RefreshTokenInvalid` and the user must re-import.
#[tauri::command(rename_all = "camelCase")]
pub async fn restore_codex_account_management(
    account_id: String,
    state: State<'_, CodexOAuthState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .restore_account_management(&account_id)
        .await
        .map_err(|e| e.to_string())
}

/// Persist an export payload to the user-chosen path. Cc-switch doesn't bundle
/// `@tauri-apps/plugin-fs`, so the React side resolves the target path via the
/// dialog plugin and hands the payload here. Uses the same `0600`-mode atomic
/// write helper as the rest of cc-switch so credentials don't leak through
/// world-readable temp files.
#[tauri::command(rename_all = "camelCase")]
pub async fn save_codex_session_export(path: String, payload: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("path is empty".to_string());
    }
    let path = std::path::PathBuf::from(trimmed);
    // Size guard mirrors MAX_IMPORT_BLOB_BYTES on the inbound side — a runaway
    // payload here would be a programming bug, not a user paste, so 4 MiB is
    // plenty.
    if payload.len() > 4 * 1024 * 1024 {
        return Err("payload too large".to_string());
    }
    crate::config::write_text_file(&path, &payload).map_err(|e| format!("write failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use serde_json::json;

    fn forge_jwt(payload: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.")
    }

    #[test]
    fn preview_returns_metadata_without_token_secrets() {
        let jwt = forge_jwt(json!({
            "exp": chrono::Utc::now().timestamp() + 3_600,
            "email": "x@y.com",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-x"}
        }));
        let cli = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": jwt, "refresh_token": "rt-secret"}
        });
        let result = preview_codex_session_parse(cli.to_string()).unwrap();
        assert_eq!(result.total, 1);
        let item = &result.items[0];
        assert_eq!(item.account_id.as_deref(), Some("acct-x"));
        assert_eq!(item.email.as_deref(), Some("x@y.com"));
        assert!(item.has_refresh_token);
        assert!(!item.is_expired);
        // Serialize and check that no token material appears in the wire format.
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("rt-secret"));
        assert!(!wire.contains(&jwt));
    }

    #[test]
    fn preview_blob_too_large_errors() {
        let huge = "a".repeat(MAX_IMPORT_BLOB_BYTES + 1);
        assert!(preview_codex_session_parse(huge).is_err());
    }

    #[test]
    fn preview_emits_warnings_when_refresh_or_account_missing() {
        // CPA file with no refresh_token, no JWT → warnings cover both gaps.
        let cpa = json!({
            "access_token": "AT",
            "type": "codex"
        });
        let result = preview_codex_session_parse(cpa.to_string()).unwrap();
        let item = &result.items[0];
        assert!(item.warnings.iter().any(|w| w.contains("refresh_token")));
        assert!(item
            .warnings
            .iter()
            .any(|w| w.contains("chatgpt_account_id")));
    }

    #[test]
    fn preview_surfaces_envelope_machine_id_and_encrypted_flag() {
        // Plaintext envelope: machine id flows through, encrypted=false.
        let plain = json!({
            "format": "cc-switch-codex-export",
            "version": 1,
            "exported_at": 1_700_000_000,
            "exported_by_machine_id": "abc123",
            "providers": []
        });
        let result = preview_codex_session_parse(plain.to_string()).unwrap();
        assert_eq!(result.envelope_source_machine_id.as_deref(), Some("abc123"));
        assert!(!result.envelope_encrypted);

        // Encrypted envelope: encrypted flag surfaces even though the
        // ciphertext doesn't decode without a password.
        let enc = json!({
            "format": "cc-switch-codex-export",
            "version": 1,
            "exported_at": 1_700_000_000,
            "encrypted": true,
            "exported_by_machine_id": "remote-host"
        });
        let result = preview_codex_session_parse(enc.to_string()).unwrap();
        assert_eq!(
            result.envelope_source_machine_id.as_deref(),
            Some("remote-host")
        );
        assert!(result.envelope_encrypted);

        // Non-envelope inputs report neither.
        let cli = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": "x"}
        });
        let result = preview_codex_session_parse(cli.to_string()).unwrap();
        assert!(result.envelope_source_machine_id.is_none());
        assert!(!result.envelope_encrypted);
    }

    fn sample_session(suffix: &str) -> CanonicalCodexSession {
        CanonicalCodexSession {
            access_token: format!("AT-{suffix}"),
            refresh_token: Some(format!("RT-{suffix}")),
            account_id: Some(format!("acct-{suffix}")),
            email: Some(format!("u{suffix}@example.com")),
            exp: Some(1_900_000_000),
            source: CodexSessionSource::CcSwitch,
            ..Default::default()
        }
    }

    #[test]
    fn render_payload_single_codex_cli_emits_auth_json_shape() {
        let sessions = vec![sample_session("1")];
        let (payload, filename) =
            render_payload(&sessions, CodexExportFormatTag::CodexCli, None, None).unwrap();
        assert_eq!(filename, "auth.json");
        assert!(payload.contains("\"auth_mode\": \"chatgpt\""));
        assert!(payload.contains("AT-1"));
    }

    #[test]
    fn render_payload_multiple_codex_cli_emits_jsonl_with_count_filename() {
        let sessions = vec![
            sample_session("1"),
            sample_session("2"),
            sample_session("3"),
        ];
        let (payload, filename) =
            render_payload(&sessions, CodexExportFormatTag::CodexCli, None, None).unwrap();
        assert_eq!(filename, "codex-sessions-3.jsonl");
        let lines: Vec<&str> = payload.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            // Each line stands alone as a parseable Codex CLI auth.json
            // (so the batch can be split/replayed line-by-line).
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(
                parsed.get("auth_mode").and_then(serde_json::Value::as_str),
                Some("chatgpt")
            );
        }
    }

    #[test]
    fn render_payload_sub2api_batch_uses_contents_array_not_jsonl() {
        let sessions = vec![sample_session("1"), sample_session("2")];
        let (payload, filename) =
            render_payload(&sessions, CodexExportFormatTag::Sub2api, None, None).unwrap();
        assert_eq!(filename, "sub2api-codex-sessions-2.json");
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let contents = value
            .get("contents")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(contents.len(), 2);
    }

    #[test]
    fn render_payload_raw_jwt_single_returns_bare_token_no_quotes() {
        let session = sample_session("solo");
        let (payload, filename) =
            render_payload(&[session.clone()], CodexExportFormatTag::RawJwt, None, None).unwrap();
        assert_eq!(payload, session.access_token);
        assert_eq!(filename, "codex-access-token.jwt");
    }

    #[test]
    fn render_payload_envelope_is_self_describing() {
        let sessions = vec![sample_session("1")];
        let (payload, filename) = render_payload(
            &sessions,
            CodexExportFormatTag::CcSwitchEnvelope,
            Some("machine-x"),
            None,
        )
        .unwrap();
        assert!(filename.starts_with("cc-switch-codex-export-"));
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            value.get("format").and_then(serde_json::Value::as_str),
            Some("cc-switch-codex-export")
        );
        assert_eq!(
            value
                .get("exported_by_machine_id")
                .and_then(serde_json::Value::as_str),
            Some("machine-x")
        );
        // Envelope re-parses round-trip through parse_many → identity preserved.
        let parsed = parse_many(&payload);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].as_ref().unwrap().account_id.as_deref(),
            Some("acct-1")
        );
    }

    #[test]
    fn build_sub2api_curl_uses_explicit_placeholders_and_heredoc() {
        let body = r#"{"contents":["payload"]}"#;
        let curl = build_sub2api_curl(body);
        // The user MUST replace these — having literal placeholders in caps
        // makes a forgotten edit fail loudly instead of silently hitting
        // someone else's instance.
        assert!(curl.contains("<SUB2API-HOST>"));
        assert!(curl.contains("<ADMIN-TOKEN>"));
        // Body is passed via heredoc so JSON isn't reinterpreted by the shell.
        assert!(curl.contains("<<'CCSWITCH_PAYLOAD_EOF'"));
        assert!(curl.contains("CCSWITCH_PAYLOAD_EOF"));
        assert!(curl.contains(body));
    }
}
