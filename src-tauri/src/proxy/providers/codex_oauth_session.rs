//! Codex Session Import/Export Utilities
//!
//! Pure parsing & serialization between cc-switch's canonical Codex OAuth
//! session representation and the external formats used by:
//!
//! - **Codex CLI** (`~/.codex/auth.json`): nested `tokens.{access,refresh,id}`
//!   with `auth_mode = "chatgpt"`.
//! - **CLIProxyAPI (CPA)** (`auths/codex-*.json`): flat `CodexTokenStorage`
//!   with `type = "codex"` and RFC3339 `expired`.
//! - **sub2api** admin import body: `{ content[s]: "<A or B>" }` accepting
//!   either of the above (or a bare access_token).
//! - **Raw access_token**: single JWT string.
//!
//! No I/O, no async, no manager dependency — wiring to `CodexOAuthManager`
//! lives in `codex_oauth_auth.rs`. Designed so the same logic round-trips
//! cleanly: import a CPA file, export back to CPA, files are equivalent
//! modulo timestamps.
//!
//! Identity keys (`identity_keys`) follow sub2api's scheme so a session
//! imported here and a session imported into sub2api dedup against each
//! other on the same anchors (account/user/email/access-token fingerprint).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Clock-skew tolerance when judging `exp`: ±2 minutes. Matches sub2api.
pub const CODEX_IMPORT_CLOCK_SKEW_SECS: i64 = 120;

/// Producer that emitted this session, used for diagnostics / UI labelling
/// and for choosing sensible export defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexSessionSource {
    /// Codex CLI `~/.codex/auth.json` (nested `tokens`, `auth_mode = chatgpt`).
    CodexCli,
    /// CLIProxyAPI `CodexTokenStorage` (flat, `type = codex`).
    Cpa,
    /// sub2api admin import body wrapping A or B.
    Sub2api,
    /// Bare access_token JWT string with no envelope.
    RawJwt,
    /// cc-switch's own internal backup envelope.
    CcSwitch,
    /// Could not be confidently classified.
    Unknown,
}

impl Default for CodexSessionSource {
    fn default() -> Self {
        CodexSessionSource::Unknown
    }
}

impl CodexSessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CodexSessionSource::CodexCli => "codex_cli",
            CodexSessionSource::Cpa => "cpa",
            CodexSessionSource::Sub2api => "sub2api",
            CodexSessionSource::RawJwt => "raw_jwt",
            CodexSessionSource::CcSwitch => "cc_switch",
            CodexSessionSource::Unknown => "unknown",
        }
    }
}

/// Target format for export. `CcSwitch` (envelope) and `Jsonl` (batch wrapper)
/// are handled by the batch renderer; here we keep per-item formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexExportFormat {
    /// Codex CLI `auth.json` shape (nested tokens, auth_mode=chatgpt).
    CodexCli,
    /// CLIProxyAPI `CodexTokenStorage` (flat).
    Cpa,
    /// sub2api admin import request body wrapping a serialized Codex CLI file.
    Sub2api,
    /// Raw access_token only.
    RawJwt,
}

/// Canonical mid-form used everywhere inside cc-switch when reasoning about
/// an imported/exported Codex session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalCodexSession {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub organization_id: Option<String>,
    /// JWT `exp` of access_token, unix seconds.
    pub exp: Option<i64>,
    /// Producer's `last_refresh` (wall-clock seconds since epoch).
    pub last_refresh: Option<i64>,
    pub source: CodexSessionSource,
    /// Carry-through of unrecognized fields so round-tripping keeps payload.
    /// `BTreeMap` for stable iteration order (important for golden-file tests).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, Value>,
}

impl CanonicalCodexSession {
    /// SHA-256 of the access_token, hex. Matches sub2api's fingerprint shape.
    pub fn access_token_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.access_token.trim().as_bytes());
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Identity anchors used to dedup against existing imports across tools.
    ///
    /// Order matches sub2api's preference list so cross-tool dedup is stable:
    /// 1. `account:<chatgpt_account_id>` (strongest — survives email changes)
    /// 2. `user:<chatgpt_user_id>` (fallback when account_id missing)
    /// 3. `email:<lowercased>` (only when no opaque IDs known)
    /// 4. `access:<sha256(access_token)>` (last resort)
    pub fn identity_keys(&self) -> Vec<String> {
        let mut keys = Vec::with_capacity(4);
        if let Some(id) = self.account_id.as_deref().map(str::trim) {
            if !id.is_empty() {
                keys.push(format!("account:{id}"));
            }
        }
        if let Some(id) = self.user_id.as_deref().map(str::trim) {
            if !id.is_empty() {
                keys.push(format!("user:{id}"));
            }
        }
        if self.account_id.is_none() && self.user_id.is_none() {
            if let Some(email) = self.email.as_deref().map(str::trim) {
                if !email.is_empty() {
                    keys.push(format!("email:{}", email.to_ascii_lowercase()));
                }
            }
        }
        let trimmed = self.access_token.trim();
        if !trimmed.is_empty() {
            keys.push(format!("access:{}", self.access_token_fingerprint()));
        }
        keys
    }

    /// Is `exp` already in the past (with clock-skew tolerance)?
    pub fn is_expired(&self, now_secs: i64) -> bool {
        match self.exp {
            Some(exp) => now_secs > exp + CODEX_IMPORT_CLOCK_SKEW_SECS,
            None => false,
        }
    }
}

/// Parse failure cases. Diagnostic strings are user-facing (Chinese to match
/// the rest of cc-switch's UX); the error is surfaced via Tauri commands.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CodexSessionParseError {
    #[error("输入为空")]
    Empty,
    #[error("JSON 解析失败: {0}")]
    Json(String),
    #[error("缺少 access_token")]
    MissingAccessToken,
    #[error("access_token 不是有效的 JWT")]
    InvalidJwt,
    #[error("access_token 已过期 (exp={0})")]
    Expired(i64),
    #[error("不支持的输入: {0}")]
    Unsupported(String),
}

// ───────────────────────────── format sniffing ──────────────────────────────

/// Best-effort classification of a single trimmed input.
///
/// Cheap pre-pass before `parse_one` so the UI can echo "looks like ..." while
/// the user is still typing. The full parse re-validates structure.
pub fn sniff_format(text: &str) -> CodexSessionSource {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return CodexSessionSource::Unknown;
    }
    // JWT three-segment heuristic. The signature segment may be empty
    // (alg=none / unsigned tokens that Codex test fixtures and some imports
    // emit), so require only that header & payload carry content.
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        let parts: Vec<&str> = trimmed.split('.').collect();
        if parts.len() == 3 && !parts[0].is_empty() && !parts[1].is_empty() {
            return CodexSessionSource::RawJwt;
        }
        return CodexSessionSource::Unknown;
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return CodexSessionSource::Unknown;
    };

    sniff_value(&value)
}

fn sniff_value(value: &Value) -> CodexSessionSource {
    let Some(obj) = value.as_object() else {
        if value.is_array() {
            // Sniff the first item as representative.
            return value
                .as_array()
                .and_then(|arr| arr.first())
                .map(sniff_value)
                .unwrap_or(CodexSessionSource::Unknown);
        }
        return CodexSessionSource::Unknown;
    };

    // cc-switch envelope wins outright — it tags itself.
    if obj
        .get("format")
        .and_then(Value::as_str)
        .map(|s| s == "cc-switch-codex-export")
        .unwrap_or(false)
    {
        return CodexSessionSource::CcSwitch;
    }
    // sub2api wraps payload in `content`/`contents`.
    if obj.contains_key("content") || obj.contains_key("contents") {
        return CodexSessionSource::Sub2api;
    }
    // Codex CLI uses nested `tokens` (or `auth_mode == "chatgpt"`).
    if obj.contains_key("tokens")
        || obj
            .get("auth_mode")
            .and_then(Value::as_str)
            .map(|s| s == "chatgpt")
            .unwrap_or(false)
    {
        return CodexSessionSource::CodexCli;
    }
    // CPA is flat with `type == "codex"`.
    if obj
        .get("type")
        .and_then(Value::as_str)
        .map(|s| s == "codex")
        .unwrap_or(false)
    {
        return CodexSessionSource::Cpa;
    }
    // Flat shape with at least access_token — assume CPA-ish.
    if obj.contains_key("access_token") {
        return CodexSessionSource::Cpa;
    }
    CodexSessionSource::Unknown
}

// ───────────────────────────── parsing entrypoints ──────────────────────────

/// Parse a single session from a string. Accepts any of A/B/C/D.
///
/// JSONL / arrays / sub2api `contents:[]` should go through `parse_many`.
pub fn parse_one(text: &str) -> Result<CanonicalCodexSession, CodexSessionParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CodexSessionParseError::Empty);
    }
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return parse_raw_jwt(trimmed);
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|e| CodexSessionParseError::Json(e.to_string()))?;
    parse_value(&value)
}

/// Parse any number of sessions from a blob. Handles:
///
/// - top-level JSON object → 1 item
/// - top-level JSON array → flatten one level
/// - sub2api wrapper with `contents:[strings]` → each string re-parsed
/// - JSONL (newline-delimited objects or bare access_tokens)
///
/// Per-item failures are returned as `Err` slots so the caller can report
/// row-by-row to the UI (matching sub2api's `ImportResult` shape).
pub fn parse_many(text: &str) -> Vec<Result<CanonicalCodexSession, CodexSessionParseError>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![Err(CodexSessionParseError::Empty)];
    }

    // Try whole-blob JSON first — covers single object, array, and sub2api wrapper.
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return expand_value(&value);
        }
        // Fall through to JSONL handling: a JSONL blob also starts with `{`.
    }

    // JSONL / mixed-line input. Empty lines and lines starting with `#` are skipped.
    let mut out = Vec::new();
    for raw_line in trimmed.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(parse_one(line));
    }
    if out.is_empty() {
        out.push(Err(CodexSessionParseError::Empty));
    }
    out
}

fn expand_value(value: &Value) -> Vec<Result<CanonicalCodexSession, CodexSessionParseError>> {
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.extend(expand_value(item));
            }
            if out.is_empty() {
                out.push(Err(CodexSessionParseError::Empty));
            }
            out
        }
        Value::Object(obj) => {
            // cc-switch envelope: expand `providers[].canonical` or `providers[].session`.
            if obj
                .get("format")
                .and_then(Value::as_str)
                .map(|s| s == "cc-switch-codex-export")
                .unwrap_or(false)
            {
                return expand_cc_switch_envelope(obj);
            }
            // sub2api wrapper: parse each child of `content` / `contents`.
            let mut children: Vec<&Value> = Vec::new();
            if let Some(content) = obj.get("content") {
                children.push(content);
            }
            if let Some(Value::Array(contents)) = obj.get("contents") {
                children.extend(contents.iter());
            }
            if !children.is_empty() {
                let mut out = Vec::new();
                for child in children {
                    out.extend(expand_sub2api_child(child));
                }
                if out.is_empty() {
                    out.push(Err(CodexSessionParseError::Empty));
                }
                return out;
            }
            vec![parse_value(value)]
        }
        _ => vec![Err(CodexSessionParseError::Unsupported(
            "顶层既不是对象也不是数组".to_string(),
        ))],
    }
}

fn expand_cc_switch_envelope(
    obj: &serde_json::Map<String, Value>,
) -> Vec<Result<CanonicalCodexSession, CodexSessionParseError>> {
    let providers = match obj.get("providers").and_then(Value::as_array) {
        Some(arr) => arr,
        None => {
            return vec![Err(CodexSessionParseError::Unsupported(
                "cc-switch 导出包缺少 providers 字段".to_string(),
            ))]
        }
    };
    let mut out = Vec::with_capacity(providers.len());
    for entry in providers {
        let session = entry
            .get("canonical")
            .or_else(|| entry.get("session"))
            .or(Some(entry));
        match session {
            Some(value) => out.push(parse_value(value)),
            None => out.push(Err(CodexSessionParseError::Unsupported(
                "providers[] 条目无 canonical/session 字段".to_string(),
            ))),
        }
    }
    if out.is_empty() {
        out.push(Err(CodexSessionParseError::Empty));
    }
    out
}

fn expand_sub2api_child(
    child: &Value,
) -> Vec<Result<CanonicalCodexSession, CodexSessionParseError>> {
    match child {
        Value::String(text) => parse_many(text),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.extend(expand_sub2api_child(item));
            }
            out
        }
        Value::Object(_) => vec![parse_value(child)],
        _ => vec![Err(CodexSessionParseError::Unsupported(
            "sub2api content 字段不是字符串/对象/数组".to_string(),
        ))],
    }
}

fn parse_value(value: &Value) -> Result<CanonicalCodexSession, CodexSessionParseError> {
    let obj = value
        .as_object()
        .ok_or_else(|| CodexSessionParseError::Unsupported("不是 JSON 对象".to_string()))?;

    // Detect by shape, not by trusting `type` alone — CPA writes `type:"codex"`
    // but sub2api wrappers may carry the same field accidentally.
    let has_nested_tokens = obj.get("tokens").map(Value::is_object).unwrap_or(false);
    let auth_mode_chatgpt = obj
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(|s| s == "chatgpt")
        .unwrap_or(false);

    if has_nested_tokens || auth_mode_chatgpt {
        parse_codex_cli(obj)
    } else if obj.contains_key("access_token") {
        parse_cpa_or_canonical(obj)
    } else {
        Err(CodexSessionParseError::MissingAccessToken)
    }
}

fn parse_raw_jwt(text: &str) -> Result<CanonicalCodexSession, CodexSessionParseError> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 3 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(CodexSessionParseError::InvalidJwt);
    }
    let mut session = CanonicalCodexSession {
        access_token: text.to_string(),
        source: CodexSessionSource::RawJwt,
        ..Default::default()
    };
    apply_jwt_claims(&mut session, text);
    Ok(session)
}

/// Codex CLI shape: `{ OPENAI_API_KEY?, auth_mode, tokens: { access, refresh, id }, last_refresh }`.
fn parse_codex_cli(
    obj: &serde_json::Map<String, Value>,
) -> Result<CanonicalCodexSession, CodexSessionParseError> {
    let tokens = obj.get("tokens").and_then(Value::as_object);

    let access_token = pick_string(obj, tokens, &["access_token", "accessToken"])
        .ok_or(CodexSessionParseError::MissingAccessToken)?;

    let refresh_token = pick_string(obj, tokens, &["refresh_token", "refreshToken"]);
    let id_token = pick_string(obj, tokens, &["id_token", "idToken"]);
    let email =
        pick_string(obj, tokens, &["email"]).or_else(|| pick_path_string(obj, &["user", "email"]));
    let account_id = pick_string(
        obj,
        tokens,
        &[
            "chatgpt_account_id",
            "chatgptAccountId",
            "account_id",
            "accountId",
        ],
    )
    .or_else(|| pick_path_string(obj, &["account", "id"]))
    .or_else(|| pick_path_string(obj, &["account", "chatgpt_account_id"]));
    let user_id = pick_string(
        obj,
        tokens,
        &["chatgpt_user_id", "chatgptUserId", "user_id", "userId"],
    );
    let plan_type = pick_string(obj, tokens, &["plan_type", "planType"]);
    let organization_id = pick_string(
        obj,
        tokens,
        &["organization_id", "organizationId", "org_id", "orgId"],
    );
    let last_refresh = parse_time_field(obj, &["last_refresh", "lastRefresh"]);

    let mut session = CanonicalCodexSession {
        access_token,
        refresh_token,
        id_token,
        account_id,
        user_id,
        email,
        plan_type,
        organization_id,
        last_refresh,
        source: CodexSessionSource::CodexCli,
        ..Default::default()
    };
    let access_token_for_jwt = session.access_token.clone();
    apply_jwt_claims(&mut session, &access_token_for_jwt);
    if let Some(id_token) = session.id_token.clone() {
        apply_jwt_claims(&mut session, &id_token);
    }
    collect_extras(obj, tokens, &mut session.extras);
    Ok(session)
}

/// CPA shape (flat): `{ access_token, refresh_token, id_token, account_id, email, type, expired, last_refresh }`.
fn parse_cpa_or_canonical(
    obj: &serde_json::Map<String, Value>,
) -> Result<CanonicalCodexSession, CodexSessionParseError> {
    let access_token = pick_string(obj, None, &["access_token", "accessToken"])
        .ok_or(CodexSessionParseError::MissingAccessToken)?;
    let refresh_token = pick_string(obj, None, &["refresh_token", "refreshToken"]);
    let id_token = pick_string(obj, None, &["id_token", "idToken"]);
    let email = pick_string(obj, None, &["email"]);
    let account_id = pick_string(
        obj,
        None,
        &[
            "account_id",
            "accountId",
            "chatgpt_account_id",
            "chatgptAccountId",
        ],
    );
    let user_id = pick_string(
        obj,
        None,
        &["user_id", "userId", "chatgpt_user_id", "chatgptUserId"],
    );
    let plan_type = pick_string(obj, None, &["plan_type", "planType"]);
    let organization_id = pick_string(
        obj,
        None,
        &["organization_id", "organizationId", "org_id", "orgId"],
    );
    let last_refresh = parse_time_field(obj, &["last_refresh", "lastRefresh"]);
    let exp_from_field = parse_time_field(obj, &["expired", "exp", "expires_at", "expiresAt"]);

    let source = if obj
        .get("type")
        .and_then(Value::as_str)
        .map(|s| s == "codex")
        .unwrap_or(false)
    {
        CodexSessionSource::Cpa
    } else {
        CodexSessionSource::Unknown
    };

    let mut session = CanonicalCodexSession {
        access_token,
        refresh_token,
        id_token,
        account_id,
        user_id,
        email,
        plan_type,
        organization_id,
        exp: exp_from_field,
        last_refresh,
        source,
        ..Default::default()
    };
    let access_token_for_jwt = session.access_token.clone();
    apply_jwt_claims(&mut session, &access_token_for_jwt);
    if let Some(id_token) = session.id_token.clone() {
        apply_jwt_claims(&mut session, &id_token);
    }
    collect_extras(obj, None, &mut session.extras);
    Ok(session)
}

// ───────────────────────────── serialization ────────────────────────────────

/// Render a single canonical session into the chosen on-the-wire shape.
///
/// `last_refresh` defaults to the canonical's stored value (when present) so
/// repeated export-import round-trips don't keep advancing the timestamp.
pub fn render(session: &CanonicalCodexSession, format: CodexExportFormat) -> Value {
    match format {
        CodexExportFormat::CodexCli => render_codex_cli(session),
        CodexExportFormat::Cpa => render_cpa(session),
        CodexExportFormat::Sub2api => render_sub2api(session),
        CodexExportFormat::RawJwt => Value::String(session.access_token.clone()),
    }
}

fn render_codex_cli(session: &CanonicalCodexSession) -> Value {
    let mut tokens = serde_json::Map::new();
    tokens.insert(
        "access_token".to_string(),
        Value::String(session.access_token.clone()),
    );
    if let Some(rt) = session.refresh_token.as_ref() {
        tokens.insert("refresh_token".to_string(), Value::String(rt.clone()));
    }
    if let Some(it) = session.id_token.as_ref() {
        tokens.insert("id_token".to_string(), Value::String(it.clone()));
    }
    if let Some(id) = session.account_id.as_ref() {
        tokens.insert("account_id".to_string(), Value::String(id.clone()));
    }

    let mut root = serde_json::Map::new();
    // Codex CLI persists `OPENAI_API_KEY` as null when using OAuth — explicit
    // null here keeps cc-switch's output indistinguishable from a real CLI dump.
    root.insert("OPENAI_API_KEY".to_string(), Value::Null);
    root.insert(
        "auth_mode".to_string(),
        Value::String("chatgpt".to_string()),
    );
    root.insert("tokens".to_string(), Value::Object(tokens));
    let last_refresh = session
        .last_refresh
        .map(|secs| format_rfc3339(secs))
        .unwrap_or_else(|| format_rfc3339(Utc::now().timestamp()));
    root.insert("last_refresh".to_string(), Value::String(last_refresh));
    Value::Object(root)
}

fn render_cpa(session: &CanonicalCodexSession) -> Value {
    let mut root = serde_json::Map::new();
    if let Some(it) = session.id_token.as_ref() {
        root.insert("id_token".to_string(), Value::String(it.clone()));
    }
    root.insert(
        "access_token".to_string(),
        Value::String(session.access_token.clone()),
    );
    if let Some(rt) = session.refresh_token.as_ref() {
        root.insert("refresh_token".to_string(), Value::String(rt.clone()));
    }
    if let Some(id) = session.account_id.as_ref() {
        root.insert("account_id".to_string(), Value::String(id.clone()));
    }
    let last_refresh = session
        .last_refresh
        .map(format_rfc3339)
        .unwrap_or_else(|| format_rfc3339(Utc::now().timestamp()));
    root.insert("last_refresh".to_string(), Value::String(last_refresh));
    if let Some(email) = session.email.as_ref() {
        root.insert("email".to_string(), Value::String(email.clone()));
    }
    root.insert("type".to_string(), Value::String("codex".to_string()));
    if let Some(exp) = session.exp {
        root.insert("expired".to_string(), Value::String(format_rfc3339(exp)));
    }
    Value::Object(root)
}

fn render_sub2api(session: &CanonicalCodexSession) -> Value {
    let inner = render_codex_cli(session);
    json!({
        "content": serde_json::to_string(&inner).unwrap_or_default(),
        "name": session
            .email
            .clone()
            .or_else(|| session.account_id.clone())
            .unwrap_or_else(|| "codex-import".to_string()),
    })
}

/// Render many sessions as JSONL. Empty input yields an empty string (callers
/// should treat that as "nothing to export" rather than as a malformed blob).
pub fn render_many_jsonl(sessions: &[CanonicalCodexSession], format: CodexExportFormat) -> String {
    let mut out = String::new();
    for session in sessions {
        let value = render(session, format);
        let line = serde_json::to_string(&value).unwrap_or_default();
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Wrap many sessions inside cc-switch's own export envelope. The envelope
/// tags itself (`format = "cc-switch-codex-export"`) so re-importers can
/// detect it without sniffing structure and so re-imports preserve sub-fields
/// (group bindings, label, etc.) that A/B/D/JSONL would drop on the way out.
///
/// `exported_at` is taken as a parameter (not read from the clock) so callers
/// in tests can pin it and so the runtime can pass a single timestamp across a
/// multi-session export batch.
pub fn render_cc_switch_envelope(
    sessions: &[CanonicalCodexSession],
    exported_at: i64,
    machine_id: Option<&str>,
) -> Value {
    let providers: Vec<Value> = sessions
        .iter()
        .map(|session| {
            json!({
                "canonical": serde_json::to_value(session).unwrap_or(Value::Null),
            })
        })
        .collect();
    let mut root = envelope_header(exported_at, machine_id);
    root.insert("providers".to_string(), Value::Array(providers));
    Value::Object(root)
}

fn envelope_header(exported_at: i64, machine_id: Option<&str>) -> serde_json::Map<String, Value> {
    let mut root = serde_json::Map::new();
    root.insert(
        "format".to_string(),
        Value::String("cc-switch-codex-export".to_string()),
    );
    root.insert("version".to_string(), Value::Number(1.into()));
    root.insert("exported_at".to_string(), Value::Number(exported_at.into()));
    if let Some(id) = machine_id {
        root.insert(
            "exported_by_machine_id".to_string(),
            Value::String(id.to_string()),
        );
    }
    root
}

/// Errors from the optional envelope encryption path. Failure modes split
/// across "could not prepare the envelope" (key derivation / RNG) and "could
/// not unseal" (wrong password / tampered ciphertext) so callers can render
/// distinct UI for each.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CodexEnvelopeCryptoError {
    #[error("密码不能为空")]
    EmptyPassword,
    #[error("密钥派生失败: {0}")]
    KeyDerivation(String),
    #[error("加密失败: {0}")]
    Encryption(String),
    #[error("envelope 已加密但未提供密码")]
    PasswordRequired,
    #[error("解密失败：密码错误或文件已被篡改")]
    Decryption,
    #[error("envelope 不是合法的加密 cc-switch 备份: {0}")]
    Malformed(String),
}

const CCSWITCH_ENVELOPE_KDF: &str = "argon2id:v19";
const CCSWITCH_ENVELOPE_CIPHER: &str = "xchacha20poly1305";
const CCSWITCH_ENVELOPE_KDF_SALT_LEN: usize = 16;
const CCSWITCH_ENVELOPE_NONCE_LEN: usize = 24;
const CCSWITCH_ENVELOPE_KEY_LEN: usize = 32;

/// Wrap many sessions inside an encrypted cc-switch envelope. The outer
/// envelope keeps `format / version / exported_at / kdf / nonce` plaintext so
/// receivers can tell at a glance what they're looking at and pick the right
/// password prompt; only the `providers[]` array is sealed with
/// XChaCha20-Poly1305 keyed via Argon2id.
///
/// `password` must be non-empty. Salt and nonce come from `OsRng`.
pub fn render_encrypted_cc_switch_envelope(
    sessions: &[CanonicalCodexSession],
    exported_at: i64,
    machine_id: Option<&str>,
    password: &str,
) -> Result<Value, CodexEnvelopeCryptoError> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use rand::RngCore;

    if password.is_empty() {
        return Err(CodexEnvelopeCryptoError::EmptyPassword);
    }

    let plaintext = serde_json::to_vec(
        &sessions
            .iter()
            .map(|session| json!({"canonical": session}))
            .collect::<Vec<_>>(),
    )
    .map_err(|e| CodexEnvelopeCryptoError::Encryption(e.to_string()))?;

    let mut salt = [0u8; CCSWITCH_ENVELOPE_KDF_SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut nonce_bytes = [0u8; CCSWITCH_ENVELOPE_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let key = derive_envelope_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new(key.as_ref().into());
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plaintext.as_ref())
        .map_err(|e| CodexEnvelopeCryptoError::Encryption(e.to_string()))?;

    let mut root = envelope_header(exported_at, machine_id);
    root.insert("encrypted".to_string(), Value::Bool(true));
    root.insert(
        "kdf".to_string(),
        json!({
            "algo": CCSWITCH_ENVELOPE_KDF,
            "salt": B64.encode(salt),
        }),
    );
    root.insert(
        "cipher".to_string(),
        Value::String(CCSWITCH_ENVELOPE_CIPHER.to_string()),
    );
    root.insert("nonce".to_string(), Value::String(B64.encode(nonce_bytes)));
    root.insert(
        "ciphertext".to_string(),
        Value::String(B64.encode(&ciphertext)),
    );
    Ok(Value::Object(root))
}

/// Decrypt an envelope produced by `render_encrypted_cc_switch_envelope`,
/// returning the canonical sessions ready for `parse_value`-style consumption.
///
/// Plaintext envelopes (no `encrypted: true`) flow through unchanged: the
/// returned sessions are just the parsed `providers[].canonical` values.
pub fn decrypt_cc_switch_envelope(
    value: &Value,
    password: Option<&str>,
) -> Result<Vec<CanonicalCodexSession>, CodexEnvelopeCryptoError> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let obj = value
        .as_object()
        .ok_or_else(|| CodexEnvelopeCryptoError::Malformed("not a JSON object".to_string()))?;
    let is_encrypted = obj
        .get("encrypted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !is_encrypted {
        // Plaintext path: re-use parse_many's envelope expansion.
        let parsed = parse_many(
            &serde_json::to_string(value)
                .map_err(|e| CodexEnvelopeCryptoError::Malformed(e.to_string()))?,
        );
        let mut out = Vec::with_capacity(parsed.len());
        for r in parsed {
            match r {
                Ok(s) => out.push(s),
                Err(e) => return Err(CodexEnvelopeCryptoError::Malformed(e.to_string())),
            }
        }
        return Ok(out);
    }

    let password = password.ok_or(CodexEnvelopeCryptoError::PasswordRequired)?;
    if password.is_empty() {
        return Err(CodexEnvelopeCryptoError::EmptyPassword);
    }

    let kdf = obj
        .get("kdf")
        .and_then(Value::as_object)
        .ok_or_else(|| CodexEnvelopeCryptoError::Malformed("missing kdf".to_string()))?;
    let kdf_algo = kdf.get("algo").and_then(Value::as_str).unwrap_or("");
    if kdf_algo != CCSWITCH_ENVELOPE_KDF {
        return Err(CodexEnvelopeCryptoError::Malformed(format!(
            "unsupported kdf algorithm: {kdf_algo}"
        )));
    }
    let salt_b64 = kdf
        .get("salt")
        .and_then(Value::as_str)
        .ok_or_else(|| CodexEnvelopeCryptoError::Malformed("missing kdf.salt".to_string()))?;
    let salt = B64
        .decode(salt_b64)
        .map_err(|e| CodexEnvelopeCryptoError::Malformed(format!("salt base64: {e}")))?;

    let cipher_algo = obj.get("cipher").and_then(Value::as_str).unwrap_or("");
    if cipher_algo != CCSWITCH_ENVELOPE_CIPHER {
        return Err(CodexEnvelopeCryptoError::Malformed(format!(
            "unsupported cipher: {cipher_algo}"
        )));
    }
    let nonce_b64 = obj
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or_else(|| CodexEnvelopeCryptoError::Malformed("missing nonce".to_string()))?;
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| CodexEnvelopeCryptoError::Malformed(format!("nonce base64: {e}")))?;
    if nonce_bytes.len() != CCSWITCH_ENVELOPE_NONCE_LEN {
        return Err(CodexEnvelopeCryptoError::Malformed(
            "nonce length mismatch".to_string(),
        ));
    }
    let ciphertext_b64 = obj
        .get("ciphertext")
        .and_then(Value::as_str)
        .ok_or_else(|| CodexEnvelopeCryptoError::Malformed("missing ciphertext".to_string()))?;
    let ciphertext = B64
        .decode(ciphertext_b64)
        .map_err(|e| CodexEnvelopeCryptoError::Malformed(format!("ciphertext base64: {e}")))?;

    let key = derive_envelope_key(password, &salt)?;
    let cipher = XChaCha20Poly1305::new(key.as_ref().into());
    let plaintext = cipher
        .decrypt(XNonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| CodexEnvelopeCryptoError::Decryption)?;

    let providers: Vec<Value> = serde_json::from_slice(&plaintext)
        .map_err(|e| CodexEnvelopeCryptoError::Malformed(format!("plaintext json: {e}")))?;
    let mut out = Vec::with_capacity(providers.len());
    for entry in providers {
        let session_value = entry
            .get("canonical")
            .or_else(|| entry.get("session"))
            .cloned()
            .ok_or_else(|| {
                CodexEnvelopeCryptoError::Malformed(
                    "providers[] entry missing canonical/session".to_string(),
                )
            })?;
        let session: CanonicalCodexSession = serde_json::from_value(session_value)
            .map_err(|e| CodexEnvelopeCryptoError::Malformed(e.to_string()))?;
        out.push(session);
    }
    Ok(out)
}

fn derive_envelope_key(
    password: &str,
    salt: &[u8],
) -> Result<[u8; CCSWITCH_ENVELOPE_KEY_LEN], CodexEnvelopeCryptoError> {
    use argon2::{Algorithm, Argon2, Params, Version};

    // OWASP 2024 baseline params for interactive uses: m=64MiB, t=3, p=4.
    // Anything cheaper risks GPU brute-force on the exported file; anything
    // beefier makes export/decrypt on lower-end laptops feel laggy.
    let params = Params::new(64 * 1024, 3, 4, Some(CCSWITCH_ENVELOPE_KEY_LEN))
        .map_err(|e| CodexEnvelopeCryptoError::KeyDerivation(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; CCSWITCH_ENVELOPE_KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| CodexEnvelopeCryptoError::KeyDerivation(e.to_string()))?;
    Ok(out)
}

/// Wrap many sessions in sub2api's `{contents:[...]}` batch import shape.
/// Each child is the inner Codex CLI JSON serialized as a string, matching
/// what sub2api's `parseCodexSessionImportEntries` actually accepts.
pub fn render_sub2api_batch(sessions: &[CanonicalCodexSession]) -> Value {
    let contents: Vec<Value> = sessions
        .iter()
        .map(|session| {
            let inner = render_codex_cli(session);
            Value::String(serde_json::to_string(&inner).unwrap_or_default())
        })
        .collect();
    json!({
        "contents": contents,
    })
}

/// Replace all token material in a copy of the session with stable, opaque
/// placeholders shaped `"<redacted:sha256:<first-12-hex>>"`. Used to produce
/// "shape-correct but unusable" export payloads for debug sharing.
///
/// The redacted copy keeps every other field (account_id, email, exp, ...)
/// so the receiver can still see what was exported, just not use it. The
/// renderer is unchanged — it serializes whatever string is in the token
/// fields — so redaction is just a transform on the canonical struct.
pub fn redact_session(session: &CanonicalCodexSession) -> CanonicalCodexSession {
    let mut redacted = session.clone();
    redacted.access_token = redact_token(&session.access_token);
    redacted.refresh_token = session.refresh_token.as_deref().map(redact_token);
    redacted.id_token = session.id_token.as_deref().map(redact_token);
    redacted
}

fn redact_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::from("<redacted:empty>");
    }
    let mut hasher = Sha256::new();
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    let mut hex_short = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex_short.push_str(&format!("{byte:02x}"));
    }
    format!("<redacted:sha256:{hex_short}>")
}

/// Suggest a target filename for a single-session export. Matches CPA's
/// `internal/auth/codex/filename.go` conventions for B-format so files saved
/// from cc-switch drop straight into a CPA `auths/` directory.
pub fn suggest_single_filename(
    session: &CanonicalCodexSession,
    format: CodexExportFormat,
) -> String {
    match format {
        CodexExportFormat::CodexCli => "auth.json".to_string(),
        CodexExportFormat::Cpa => {
            let stem = session
                .email
                .as_deref()
                .map(sanitize_filename_stem)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    session
                        .account_id
                        .as_deref()
                        .map(sanitize_filename_stem)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "codex".to_string());
            format!("codex-{stem}.json")
        }
        CodexExportFormat::Sub2api => "codex-session-import.json".to_string(),
        CodexExportFormat::RawJwt => "codex-access-token.jwt".to_string(),
    }
}

/// Suggest a target filename for a batch export.
pub fn suggest_batch_filename(format: CodexExportFormat, count: usize) -> String {
    match format {
        CodexExportFormat::CodexCli => format!("codex-sessions-{count}.jsonl"),
        CodexExportFormat::Cpa => format!("cpa-codex-sessions-{count}.jsonl"),
        CodexExportFormat::Sub2api => format!("sub2api-codex-sessions-{count}.json"),
        CodexExportFormat::RawJwt => format!("codex-access-tokens-{count}.txt"),
    }
}

/// Suggest a filename for the cc-switch envelope batch.
pub fn suggest_envelope_filename(exported_at: i64) -> String {
    let stamp = format_rfc3339(exported_at).replace(':', "-");
    format!("cc-switch-codex-export-{stamp}.json")
}

fn sanitize_filename_stem(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            '@' => '_',
            _ => '-',
        })
        .collect::<String>()
        .trim_matches(|c: char| c == '-' || c == '.')
        .to_string()
}

// ───────────────────────────── helpers ──────────────────────────────────────

/// Decode the `exp` claim of a JWT (base64url payload, no signature check).
/// Returns `None` if the token is malformed or has no numeric `exp`.
pub fn decode_jwt_exp(token: &str) -> Option<i64> {
    decode_jwt_payload(token)
        .and_then(|value| value.get("exp").cloned())
        .and_then(|exp| match exp {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.parse::<i64>().ok(),
            _ => None,
        })
}

fn decode_jwt_payload(token: &str) -> Option<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// Merge any non-empty `exp/email/account_id/...` claims from a JWT payload
/// into the canonical session. JWT is the source of truth for `exp`; other
/// fields fill in only when the canonical doesn't already have them, so
/// fields lifted from the producer's envelope win over JWT-derived ones.
fn apply_jwt_claims(session: &mut CanonicalCodexSession, token: &str) {
    let Some(payload) = decode_jwt_payload(token) else {
        return;
    };
    let payload = match payload.as_object() {
        Some(obj) => obj,
        None => return,
    };

    if session.exp.is_none() {
        if let Some(n) = payload.get("exp").and_then(Value::as_i64) {
            session.exp = Some(n);
        }
    }
    if session.email.is_none() {
        if let Some(s) = payload.get("email").and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                session.email = Some(trimmed.to_string());
            }
        }
    }

    // Codex-specific custom claim — both `claims["https://api.openai.com/auth"]`
    // and a flat `chatgpt_account_id` are observed in the wild; check both.
    let openai = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);

    let pick = |key: &str| -> Option<String> {
        openai
            .and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| payload.get(key).and_then(Value::as_str).map(str::to_string))
    };

    if session.account_id.is_none() {
        session.account_id = pick("chatgpt_account_id");
    }
    if session.user_id.is_none() {
        session.user_id = pick("chatgpt_user_id").or_else(|| pick("user_id"));
    }
    if session.user_id.is_none() {
        // Fall back to JWT's `sub` only if nothing else identified the user.
        if let Some(sub) = payload.get("sub").and_then(Value::as_str) {
            let trimmed = sub.trim();
            if !trimmed.is_empty() {
                session.user_id = Some(trimmed.to_string());
            }
        }
    }
    if session.plan_type.is_none() {
        session.plan_type = pick("chatgpt_plan_type").or_else(|| pick("plan_type"));
    }
    if session.organization_id.is_none() {
        session.organization_id = pick("poid").or_else(|| pick("organization_id"));
        // organizations[] fallback: prefer is_default, else first.
        if session.organization_id.is_none() {
            if let Some(orgs) = openai
                .and_then(|o| o.get("organizations"))
                .and_then(Value::as_array)
            {
                let mut chosen: Option<String> = None;
                for org in orgs {
                    let id = org.get("id").and_then(Value::as_str).map(str::to_string);
                    if id.is_none() {
                        continue;
                    }
                    let is_default = org
                        .get("is_default")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if is_default {
                        chosen = id;
                        break;
                    }
                    if chosen.is_none() {
                        chosen = id;
                    }
                }
                session.organization_id = chosen;
            }
        }
    }
}

/// Look up a string at one of several keys; prefer the nested `tokens` table
/// when present, then fall back to the flat object.
fn pick_string(
    obj: &serde_json::Map<String, Value>,
    nested: Option<&serde_json::Map<String, Value>>,
    keys: &[&str],
) -> Option<String> {
    for key in keys {
        if let Some(t) = nested {
            if let Some(s) = t.get(*key).and_then(Value::as_str) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        if let Some(s) = obj.get(*key).and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn pick_path_string(obj: &serde_json::Map<String, Value>, path: &[&str]) -> Option<String> {
    let mut cursor: &Value = obj.get(*path.first()?)?;
    for key in path.iter().skip(1) {
        cursor = cursor.get(*key)?;
    }
    cursor
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse a unix-second timestamp from a JSON field shaped as RFC3339 string
/// or as an integer/float. Used for `exp / expired / last_refresh`.
fn parse_time_field(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match obj.get(*key) {
            Some(Value::String(s)) => {
                if let Ok(parsed) = DateTime::parse_from_rfc3339(s) {
                    return Some(parsed.timestamp());
                }
                if let Ok(n) = s.parse::<i64>() {
                    return Some(normalize_unix_ts(n));
                }
            }
            Some(Value::Number(n)) => {
                if let Some(i) = n.as_i64() {
                    return Some(normalize_unix_ts(i));
                }
                if let Some(f) = n.as_f64() {
                    return Some(normalize_unix_ts(f as i64));
                }
            }
            _ => {}
        }
    }
    None
}

fn normalize_unix_ts(value: i64) -> i64 {
    // Treat clearly-millisecond magnitudes (year 33658+) as ms; otherwise s.
    if value > 1_000_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn format_rfc3339(secs: i64) -> String {
    Utc.timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Sweep up anything not explicitly recognized into `extras` so round-trips
/// don't drop carry-through fields. Keys hardcoded here mirror those already
/// projected into the canonical struct.
fn collect_extras(
    obj: &serde_json::Map<String, Value>,
    nested: Option<&serde_json::Map<String, Value>>,
    extras: &mut BTreeMap<String, Value>,
) {
    const KNOWN: &[&str] = &[
        "access_token",
        "accessToken",
        "refresh_token",
        "refreshToken",
        "id_token",
        "idToken",
        "tokens",
        "auth_mode",
        "OPENAI_API_KEY",
        "account_id",
        "accountId",
        "chatgpt_account_id",
        "chatgptAccountId",
        "user_id",
        "userId",
        "chatgpt_user_id",
        "chatgptUserId",
        "email",
        "plan_type",
        "planType",
        "organization_id",
        "organizationId",
        "org_id",
        "orgId",
        "type",
        "expired",
        "exp",
        "expires_at",
        "expiresAt",
        "last_refresh",
        "lastRefresh",
        "content",
        "contents",
        "name",
    ];
    for (key, value) in obj {
        if KNOWN.contains(&key.as_str()) {
            continue;
        }
        if value.is_null() {
            continue;
        }
        extras.insert(key.clone(), value.clone());
    }
    if let Some(t) = nested {
        for (key, value) in t {
            if KNOWN.contains(&key.as_str()) {
                continue;
            }
            if value.is_null() {
                continue;
            }
            // Prefix nested keys so they don't collide with top-level extras.
            extras
                .entry(format!("tokens.{key}"))
                .or_insert(value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal forged JWT (alg=none) carrying the supplied payload.
    fn forge_jwt(payload: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{payload_b64}.")
    }

    #[test]
    fn sniff_known_shapes() {
        assert_eq!(sniff_format(""), CodexSessionSource::Unknown);
        assert_eq!(sniff_format("not-json"), CodexSessionSource::Unknown);
        let jwt = forge_jwt(json!({"exp": 100}));
        assert_eq!(sniff_format(&jwt), CodexSessionSource::RawJwt);
        assert_eq!(
            sniff_format("{\"tokens\":{\"access_token\":\"x\"}}"),
            CodexSessionSource::CodexCli
        );
        assert_eq!(
            sniff_format("{\"type\":\"codex\",\"access_token\":\"x\"}"),
            CodexSessionSource::Cpa
        );
        assert_eq!(
            sniff_format("{\"content\":\"x\"}"),
            CodexSessionSource::Sub2api
        );
        assert_eq!(
            sniff_format("{\"format\":\"cc-switch-codex-export\",\"providers\":[]}"),
            CodexSessionSource::CcSwitch
        );
    }

    #[test]
    fn parse_raw_jwt_extracts_exp_and_email() {
        let jwt = forge_jwt(json!({
            "exp": 1_900_000_000_i64,
            "email": "a@b.com",
            "sub": "user-1",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-1",
                "chatgpt_plan_type": "plus",
            }
        }));
        let session = parse_one(&jwt).unwrap();
        assert_eq!(session.source, CodexSessionSource::RawJwt);
        assert_eq!(session.exp, Some(1_900_000_000));
        assert_eq!(session.email.as_deref(), Some("a@b.com"));
        assert_eq!(session.account_id.as_deref(), Some("acct-1"));
        assert_eq!(session.plan_type.as_deref(), Some("plus"));
        assert_eq!(session.user_id.as_deref(), Some("user-1"));
    }

    #[test]
    fn parse_codex_cli_shape_round_trip() {
        let jwt = forge_jwt(json!({"exp": 1_900_000_001_i64}));
        let input = json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": jwt,
                "refresh_token": "rt-xyz",
                "id_token": forge_jwt(json!({
                    "email": "u@example.com",
                    "https://api.openai.com/auth": {"chatgpt_account_id": "acct-9"}
                })),
                "account_id": "acct-9"
            },
            "last_refresh": "2024-01-02T03:04:05Z"
        });
        let session = parse_one(&input.to_string()).unwrap();
        assert_eq!(session.source, CodexSessionSource::CodexCli);
        assert_eq!(session.refresh_token.as_deref(), Some("rt-xyz"));
        assert_eq!(session.account_id.as_deref(), Some("acct-9"));
        assert_eq!(session.email.as_deref(), Some("u@example.com"));
        assert_eq!(session.exp, Some(1_900_000_001));

        // Re-render as CodexCli and parse again: identity keys must match.
        let rendered = render(&session, CodexExportFormat::CodexCli);
        let session2 = parse_one(&rendered.to_string()).unwrap();
        assert_eq!(session.identity_keys(), session2.identity_keys());
    }

    #[test]
    fn parse_cpa_flat_shape() {
        let jwt = forge_jwt(json!({"exp": 1_900_000_002_i64}));
        let input = json!({
            "id_token": "id-token",
            "access_token": jwt,
            "refresh_token": "rt-cpa",
            "account_id": "acct-cpa",
            "last_refresh": "2024-05-06T07:08:09Z",
            "email": "cpa@example.com",
            "type": "codex",
            "expired": "2099-12-31T23:59:59Z"
        });
        let session = parse_one(&input.to_string()).unwrap();
        assert_eq!(session.source, CodexSessionSource::Cpa);
        assert_eq!(session.refresh_token.as_deref(), Some("rt-cpa"));
        assert_eq!(session.account_id.as_deref(), Some("acct-cpa"));
        assert_eq!(session.email.as_deref(), Some("cpa@example.com"));
        // exp field on CPA wins over JWT-derived value
        assert!(session.exp.unwrap() > 4_000_000_000);
    }

    #[test]
    fn parse_sub2api_wrapper_with_string_content() {
        let inner = json!({
            "tokens": {"access_token": forge_jwt(json!({"exp": 1_900_000_010_i64}))},
            "auth_mode": "chatgpt"
        })
        .to_string();
        let wrapper = json!({"content": inner, "name": "imported"}).to_string();
        let results = parse_many(&wrapper);
        assert_eq!(results.len(), 1);
        let session = results[0].as_ref().unwrap();
        assert_eq!(session.source, CodexSessionSource::CodexCli);
    }

    #[test]
    fn parse_sub2api_wrapper_with_contents_array() {
        let cli = json!({
            "tokens": {"access_token": forge_jwt(json!({"exp": 1_900_000_020_i64}))},
            "auth_mode": "chatgpt"
        });
        let wrapper =
            json!({"contents": [cli.to_string(), cli.to_string()], "name": "bulk"}).to_string();
        let results = parse_many(&wrapper);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(Result::is_ok));
    }

    #[test]
    fn parse_jsonl_mixed_lines() {
        let jwt = forge_jwt(json!({"exp": 1_900_000_030_i64}));
        let blob = format!(
            "# comment line\n\n{}\n{}\n",
            jwt,
            json!({"tokens": {"access_token": jwt.clone()}, "auth_mode": "chatgpt"}).to_string()
        );
        let results = parse_many(&blob);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(
            results[0].as_ref().unwrap().source,
            CodexSessionSource::RawJwt
        );
        assert_eq!(
            results[1].as_ref().unwrap().source,
            CodexSessionSource::CodexCli
        );
    }

    #[test]
    fn parse_top_level_array_flattens() {
        let cli = json!({
            "tokens": {"access_token": forge_jwt(json!({"exp": 1_900_000_040_i64}))},
            "auth_mode": "chatgpt"
        });
        let arr = json!([cli, cli]);
        let results = parse_many(&arr.to_string());
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn parse_missing_access_token_errors_clearly() {
        let input = json!({"type": "codex", "email": "x@y.com"});
        let err = parse_one(&input.to_string()).unwrap_err();
        assert!(matches!(err, CodexSessionParseError::MissingAccessToken));
    }

    #[test]
    fn parse_invalid_jwt_string_errors_clearly() {
        let err = parse_one("a.b").unwrap_err();
        assert!(matches!(err, CodexSessionParseError::InvalidJwt));
    }

    #[test]
    fn identity_keys_prefer_account_then_user_then_email() {
        let mut s = CanonicalCodexSession {
            access_token: "AT".to_string(),
            ..Default::default()
        };
        assert_eq!(s.identity_keys().len(), 1, "fingerprint only");
        s.email = Some("X@Y.COM".to_string());
        let keys = s.identity_keys();
        assert!(keys.iter().any(|k| k == "email:x@y.com"), "{keys:?}");
        s.user_id = Some("u".to_string());
        let keys = s.identity_keys();
        assert!(keys.iter().any(|k| k.starts_with("user:")));
        // user/account present → email no longer projected as a key
        assert!(!keys.iter().any(|k| k.starts_with("email:")));
        s.account_id = Some("acct".to_string());
        let keys = s.identity_keys();
        assert_eq!(keys[0], "account:acct");
    }

    #[test]
    fn extras_round_trip_carries_unknown_fields() {
        let input = json!({
            "tokens": {"access_token": forge_jwt(json!({"exp": 1_900_000_050_i64})), "custom_field": "v"},
            "auth_mode": "chatgpt",
            "arbitrary": {"keep_me": true}
        });
        let session = parse_one(&input.to_string()).unwrap();
        assert!(session.extras.contains_key("arbitrary"));
        assert!(session.extras.contains_key("tokens.custom_field"));
    }

    #[test]
    fn render_codex_cli_emits_shape_codex_can_read() {
        let jwt = forge_jwt(json!({"exp": 1_900_000_060_i64}));
        let session = CanonicalCodexSession {
            access_token: jwt.clone(),
            refresh_token: Some("rt".to_string()),
            id_token: Some("id".to_string()),
            account_id: Some("acct".to_string()),
            exp: Some(1_900_000_060),
            last_refresh: Some(1_700_000_000),
            source: CodexSessionSource::CodexCli,
            ..Default::default()
        };
        let value = render(&session, CodexExportFormat::CodexCli);
        assert_eq!(
            value.get("auth_mode").and_then(Value::as_str),
            Some("chatgpt")
        );
        assert!(value
            .get("OPENAI_API_KEY")
            .map(Value::is_null)
            .unwrap_or(false));
        let tokens = value.get("tokens").and_then(Value::as_object).unwrap();
        assert_eq!(
            tokens.get("access_token").and_then(Value::as_str),
            Some(jwt.as_str())
        );
        assert_eq!(
            tokens.get("refresh_token").and_then(Value::as_str),
            Some("rt")
        );
        assert_eq!(
            tokens.get("account_id").and_then(Value::as_str),
            Some("acct")
        );
    }

    #[test]
    fn render_cpa_emits_type_codex_and_rfc3339_expired() {
        let session = CanonicalCodexSession {
            access_token: "AT".to_string(),
            refresh_token: Some("RT".to_string()),
            account_id: Some("acct".to_string()),
            exp: Some(1_900_000_000),
            ..Default::default()
        };
        let value = render(&session, CodexExportFormat::Cpa);
        assert_eq!(value.get("type").and_then(Value::as_str), Some("codex"));
        let expired = value.get("expired").and_then(Value::as_str).unwrap();
        assert!(expired.ends_with('Z'));
    }

    #[test]
    fn render_sub2api_wraps_codex_cli_payload_as_string_content() {
        let session = CanonicalCodexSession {
            access_token: "AT".to_string(),
            email: Some("name@example.com".to_string()),
            ..Default::default()
        };
        let value = render(&session, CodexExportFormat::Sub2api);
        let content = value.get("content").and_then(Value::as_str).unwrap();
        assert!(content.contains("\"auth_mode\":\"chatgpt\""));
        assert_eq!(
            value.get("name").and_then(Value::as_str),
            Some("name@example.com")
        );
    }

    #[test]
    fn round_trip_codex_cli_via_cpa_preserves_identity() {
        let jwt = forge_jwt(json!({
            "exp": 1_900_000_070_i64,
            "email": "rt@example.com",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-rt"}
        }));
        let original = json!({
            "auth_mode": "chatgpt",
            "tokens": {"access_token": jwt, "refresh_token": "rt"}
        });
        let s1 = parse_one(&original.to_string()).unwrap();
        let as_cpa = render(&s1, CodexExportFormat::Cpa);
        let s2 = parse_one(&as_cpa.to_string()).unwrap();
        assert_eq!(s1.account_id, s2.account_id);
        assert_eq!(s1.email, s2.email);
        assert_eq!(s1.refresh_token, s2.refresh_token);
        assert_eq!(s1.identity_keys(), s2.identity_keys());
    }

    #[test]
    fn expired_check_respects_skew() {
        let s = CanonicalCodexSession {
            access_token: "AT".to_string(),
            exp: Some(1_000),
            ..Default::default()
        };
        assert!(!s.is_expired(1_000 + CODEX_IMPORT_CLOCK_SKEW_SECS));
        assert!(s.is_expired(1_000 + CODEX_IMPORT_CLOCK_SKEW_SECS + 1));
        let no_exp = CanonicalCodexSession {
            access_token: "AT".to_string(),
            ..Default::default()
        };
        assert!(!no_exp.is_expired(i64::MAX));
    }

    #[test]
    fn redact_session_replaces_all_token_material_deterministically() {
        let session = CanonicalCodexSession {
            access_token: "AT-original".to_string(),
            refresh_token: Some("RT-original".to_string()),
            id_token: Some("IT-original".to_string()),
            account_id: Some("acct".to_string()),
            email: Some("u@example.com".to_string()),
            ..Default::default()
        };
        let redacted = redact_session(&session);
        // Non-token metadata flows through unchanged.
        assert_eq!(redacted.account_id, session.account_id);
        assert_eq!(redacted.email, session.email);
        // All three token fields are replaced and do not contain the original.
        assert!(redacted.access_token.starts_with("<redacted:sha256:"));
        assert!(!redacted.access_token.contains("original"));
        assert!(redacted
            .refresh_token
            .as_deref()
            .unwrap()
            .starts_with("<redacted:sha256:"));
        assert!(redacted
            .id_token
            .as_deref()
            .unwrap()
            .starts_with("<redacted:sha256:"));
        // Deterministic: same input → same digest.
        let redacted2 = redact_session(&session);
        assert_eq!(redacted.access_token, redacted2.access_token);
        // Empty/whitespace token gets the empty marker, not a hash of "".
        let blank = CanonicalCodexSession {
            access_token: "   ".to_string(),
            ..Default::default()
        };
        assert_eq!(redact_session(&blank).access_token, "<redacted:empty>");
    }

    #[test]
    fn cc_switch_envelope_round_trips_through_parse_many() {
        let session = CanonicalCodexSession {
            access_token: "AT".to_string(),
            refresh_token: Some("RT".to_string()),
            account_id: Some("acct".to_string()),
            email: Some("u@example.com".to_string()),
            exp: Some(1_900_000_100),
            source: CodexSessionSource::CcSwitch,
            ..Default::default()
        };
        let envelope = render_cc_switch_envelope(&[session.clone()], 1_700_000_000, Some("mid"));
        // Envelope tags itself, so sniff recognizes it without structural guessing.
        assert_eq!(
            sniff_format(&envelope.to_string()),
            CodexSessionSource::CcSwitch
        );
        let parsed = parse_many(&envelope.to_string());
        assert_eq!(parsed.len(), 1);
        let restored = parsed[0].as_ref().unwrap();
        assert_eq!(restored.account_id, session.account_id);
        assert_eq!(restored.refresh_token, session.refresh_token);
        assert_eq!(restored.email, session.email);
    }

    #[test]
    fn sub2api_batch_renders_as_contents_array_of_strings() {
        let sessions = vec![
            CanonicalCodexSession {
                access_token: "AT-1".to_string(),
                refresh_token: Some("RT-1".to_string()),
                account_id: Some("acct-1".to_string()),
                ..Default::default()
            },
            CanonicalCodexSession {
                access_token: "AT-2".to_string(),
                refresh_token: Some("RT-2".to_string()),
                account_id: Some("acct-2".to_string()),
                ..Default::default()
            },
        ];
        let batch = render_sub2api_batch(&sessions);
        let contents = batch.get("contents").and_then(Value::as_array).unwrap();
        assert_eq!(contents.len(), 2);
        // Each child is a JSON-encoded string of the Codex CLI inner payload.
        for child in contents {
            let text = child.as_str().expect("contents[] must be string");
            assert!(text.contains("\"auth_mode\":\"chatgpt\""));
        }
    }

    #[test]
    fn filename_suggestions_match_target_conventions() {
        let session = CanonicalCodexSession {
            access_token: "AT".to_string(),
            email: Some("user+tag@example.com".to_string()),
            account_id: Some("acct-1".to_string()),
            ..Default::default()
        };
        assert_eq!(
            suggest_single_filename(&session, CodexExportFormat::CodexCli),
            "auth.json"
        );
        // `@` becomes `_`, `+` becomes `-` (anything not in the safe alphabet).
        assert_eq!(
            suggest_single_filename(&session, CodexExportFormat::Cpa),
            "codex-user-tag_example.com.json"
        );
        assert_eq!(
            suggest_single_filename(&session, CodexExportFormat::RawJwt),
            "codex-access-token.jwt"
        );
        // Falls back to account_id when email missing.
        let no_email = CanonicalCodexSession {
            access_token: "AT".to_string(),
            account_id: Some("acct-9".to_string()),
            ..Default::default()
        };
        assert_eq!(
            suggest_single_filename(&no_email, CodexExportFormat::Cpa),
            "codex-acct-9.json"
        );
        // Batch filenames carry the count to make accidental overwrites obvious.
        assert_eq!(
            suggest_batch_filename(CodexExportFormat::CodexCli, 3),
            "codex-sessions-3.jsonl"
        );
        assert_eq!(
            suggest_batch_filename(CodexExportFormat::RawJwt, 5),
            "codex-access-tokens-5.txt"
        );
    }

    fn sample_for_crypto() -> CanonicalCodexSession {
        CanonicalCodexSession {
            access_token: "AT-secret".to_string(),
            refresh_token: Some("RT-secret".to_string()),
            account_id: Some("acct-1".to_string()),
            email: Some("u@example.com".to_string()),
            exp: Some(1_900_000_500),
            source: CodexSessionSource::CcSwitch,
            ..Default::default()
        }
    }

    #[test]
    fn encrypted_envelope_round_trips_with_correct_password() {
        let session = sample_for_crypto();
        let envelope = render_encrypted_cc_switch_envelope(
            &[session.clone()],
            1_700_000_000,
            Some("mid"),
            "correct horse battery staple",
        )
        .expect("encrypt should succeed");
        let obj = envelope.as_object().unwrap();
        // Outer manifest stays plaintext so receivers know what to ask for.
        assert_eq!(
            obj.get("format").and_then(Value::as_str),
            Some("cc-switch-codex-export")
        );
        assert_eq!(obj.get("encrypted").and_then(Value::as_bool), Some(true));
        assert!(obj.get("kdf").is_some());
        assert!(obj.get("nonce").is_some());
        assert!(obj.get("ciphertext").is_some());
        // No plaintext provider data leaks into the wire.
        let wire = envelope.to_string();
        assert!(!wire.contains("AT-secret"));
        assert!(!wire.contains("RT-secret"));
        assert!(!wire.contains("u@example.com"));

        let decrypted = decrypt_cc_switch_envelope(&envelope, Some("correct horse battery staple"))
            .expect("decrypt with correct password should succeed");
        assert_eq!(decrypted.len(), 1);
        assert_eq!(decrypted[0].access_token, session.access_token);
        assert_eq!(decrypted[0].refresh_token, session.refresh_token);
        assert_eq!(decrypted[0].email, session.email);
    }

    #[test]
    fn encrypted_envelope_rejects_wrong_password() {
        let session = sample_for_crypto();
        let envelope = render_encrypted_cc_switch_envelope(&[session], 1_700_000_000, None, "pw")
            .expect("encrypt");
        let err = decrypt_cc_switch_envelope(&envelope, Some("WRONG"))
            .expect_err("wrong password must not decrypt");
        assert!(matches!(err, CodexEnvelopeCryptoError::Decryption));
    }

    #[test]
    fn encrypted_envelope_rejects_missing_password() {
        let session = sample_for_crypto();
        let envelope = render_encrypted_cc_switch_envelope(&[session], 1_700_000_000, None, "pw")
            .expect("encrypt");
        let err = decrypt_cc_switch_envelope(&envelope, None)
            .expect_err("encrypted envelope must require a password");
        assert!(matches!(err, CodexEnvelopeCryptoError::PasswordRequired));
    }

    #[test]
    fn plaintext_envelope_decrypt_passthrough_ignores_password() {
        let session = sample_for_crypto();
        let plain = render_cc_switch_envelope(&[session.clone()], 1_700_000_000, None);
        let parsed = decrypt_cc_switch_envelope(&plain, None)
            .expect("plaintext envelope decrypt should pass through");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].account_id, session.account_id);
    }

    #[test]
    fn render_encrypted_envelope_refuses_empty_password() {
        let err =
            render_encrypted_cc_switch_envelope(&[sample_for_crypto()], 1_700_000_000, None, "")
                .expect_err("empty password must be rejected");
        assert!(matches!(err, CodexEnvelopeCryptoError::EmptyPassword));
    }

    #[test]
    fn encrypted_envelope_tamper_detection_via_aead() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        let session = sample_for_crypto();
        let mut envelope =
            render_encrypted_cc_switch_envelope(&[session], 1_700_000_000, None, "pw").unwrap();
        // Flip one byte of the ciphertext.
        let obj = envelope.as_object_mut().unwrap();
        let mut bytes = B64
            .decode(obj.get("ciphertext").and_then(Value::as_str).unwrap())
            .unwrap();
        bytes[0] ^= 0x01;
        obj.insert("ciphertext".to_string(), Value::String(B64.encode(&bytes)));
        let err = decrypt_cc_switch_envelope(&envelope, Some("pw"))
            .expect_err("tampered AEAD must not decrypt");
        assert!(matches!(err, CodexEnvelopeCryptoError::Decryption));
    }
}
