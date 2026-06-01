import { invoke } from "@tauri-apps/api/core";

// ─────────────────────────── shared types ──────────────────────────────────

/**
 * Producer that emitted a session, mirrored from the Rust enum.
 * Used by the preview command to label rows in the import preview table.
 */
export type CodexSessionSource =
  | "codex_cli"
  | "cpa"
  | "sub2api"
  | "raw_jwt"
  | "cc_switch"
  | "unknown";

/** Per-format export tag, mirrored from the Rust enum used by export. */
export type CodexExportFormat =
  | "codex_cli"
  | "cpa"
  | "sub2api"
  | "raw_jwt"
  | "cc_switch_envelope";

// ─────────────────────────── preview ───────────────────────────────────────

export interface CodexSessionPreviewItem {
  index: number;
  source: CodexSessionSource;
  accountId: string | null;
  userId: string | null;
  email: string | null;
  planType: string | null;
  organizationId: string | null;
  exp: number | null;
  hasRefreshToken: boolean;
  hasIdToken: boolean;
  isExpired: boolean;
  error: string | null;
  warnings: string[];
}

export interface CodexSessionPreviewResult {
  sniffedFormat: CodexSessionSource;
  total: number;
  items: CodexSessionPreviewItem[];
  /** When the input is a cc-switch envelope, the machine id it was exported from. */
  envelopeSourceMachineId: string | null;
  /** True when the input is an `encrypted: true` cc-switch envelope. */
  envelopeEncrypted: boolean;
}

/**
 * Pure parse — no I/O, no manager writes. Used to feed a live "we detected
 * N sessions" preview as the user types or pastes. The wire format is
 * deliberately scrubbed of token material; only identity metadata returns.
 */
export async function previewCodexSessionParse(
  text: string,
): Promise<CodexSessionPreviewResult> {
  return invoke<CodexSessionPreviewResult>("preview_codex_session_parse", {
    text,
  });
}

// ─────────────────────────── import ────────────────────────────────────────

export interface CodexSessionImportRequest {
  content: string;
  /** Default true. When false, existing accounts skip rather than overwrite. */
  updateExisting?: boolean;
  /** Default true. When false, expired access_tokens are imported anyway. */
  rejectExpired?: boolean;
  /** Default false. When true, validate refresh_token by forcing a refresh. */
  verifyRefresh?: boolean;
  /** Optional password for encrypted cc-switch envelope imports. */
  password?: string;
}

export type CodexSessionImportAction = "created" | "updated" | "skipped" | "failed";

export interface CodexSessionImportItem {
  index: number;
  action: CodexSessionImportAction;
  accountId: string | null;
  email: string | null;
  message: string | null;
}

export interface CodexSessionImportMessage {
  index: number;
  accountId: string | null;
  email: string | null;
  message: string;
}

export interface CodexSessionImportResult {
  total: number;
  created: number;
  updated: number;
  skipped: number;
  failed: number;
  items: CodexSessionImportItem[];
  warnings: CodexSessionImportMessage[];
  errors: CodexSessionImportMessage[];
}

/**
 * Import sessions from any supported wire format (Codex CLI auth.json, CPA
 * `CodexTokenStorage`, sub2api wrapper, bare JWT, JSONL, or cc-switch
 * envelope — encrypted envelopes need `password`). Returns a row-by-row
 * outcome the UI can render directly.
 */
export async function importCodexSessions(
  request: CodexSessionImportRequest,
): Promise<CodexSessionImportResult> {
  return invoke<CodexSessionImportResult>("import_codex_sessions", { request });
}

// ─────────────────────────── export ────────────────────────────────────────

export interface CodexSessionExportRequest {
  /** Empty = export every managed account, in list order. */
  accountIds?: string[];
  format: CodexExportFormat;
  /** Default true. Refresh before export so downstream gets a fresh token. */
  refreshFirst?: boolean;
  /** Default false. Replace token material with deterministic SHA-256 markers. */
  redact?: boolean;
  /** Embedded into cc-switch envelope; receivers can detect cross-machine sources. */
  machineId?: string;
  /** Default false. After export, mark accounts as handed-off (stop refreshing). */
  markHandoff?: boolean;
  /** Optional password for cc-switch envelope encryption only. */
  password?: string;
}

export type CodexSessionExportStatus = "ok" | "refresh_failed" | "not_found";

export interface CodexSessionExportItem {
  accountId: string;
  email: string | null;
  status: CodexSessionExportStatus;
  exp: number | null;
  message: string | null;
}

export interface CodexSessionExportResult {
  format: CodexExportFormat;
  suggestedFilename: string;
  payload: string;
  redacted: boolean;
  accountCount: number;
  warnings: string[];
  items: CodexSessionExportItem[];
  curlCommand?: string;
}

export async function exportCodexSessions(
  request: CodexSessionExportRequest,
): Promise<CodexSessionExportResult> {
  return invoke<CodexSessionExportResult>("export_codex_sessions", { request });
}

// ─────────────────────────── handoff ───────────────────────────────────────

export async function markCodexAccountHandoff(accountId: string): Promise<void> {
  return invoke("mark_codex_account_handoff", { accountId });
}

export async function restoreCodexAccountManagement(
  accountId: string,
): Promise<void> {
  return invoke("restore_codex_account_management", { accountId });
}

/**
 * Persist an export payload to a user-chosen path via the Rust side.
 * The React caller resolves the path through `@tauri-apps/plugin-dialog`'s
 * `save()` and hands the result here — cc-switch doesn't bundle plugin-fs.
 */
export async function saveCodexSessionExport(
  path: string,
  payload: string,
): Promise<void> {
  return invoke("save_codex_session_export", { path, payload });
}

/**
 * Persistent random identifier for this cc-switch installation. Embedded
 * into envelope exports so the UI can flag "this backup came from another
 * machine" on import. Cached client-side after first call.
 */
let cachedMachineId: string | null = null;
export async function getCodexSessionMachineId(): Promise<string> {
  if (cachedMachineId !== null) return cachedMachineId;
  cachedMachineId = await invoke<string>("get_codex_session_machine_id");
  return cachedMachineId;
}
