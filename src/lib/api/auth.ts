import { invoke } from "@tauri-apps/api/core";

export type ManagedAuthProvider =
  | "github_copilot"
  | "codex_oauth"
  | "claude_oauth"
  | "google_gemini_oauth"
  | "antigravity_oauth"
  | "cursor_oauth"
  | "kiro_oauth";

export interface DeepSeekAccount {
  id: string;
  login: string;
  authenticated_at: number;
  is_default: boolean;
  has_password: boolean;
}

export interface DeepSeekAccountStatus {
  authenticated: boolean;
  default_account_id: string | null;
  accounts: DeepSeekAccount[];
}

export interface ManagedAuthAccount {
  id: string;
  provider: ManagedAuthProvider;
  login: string;
  avatar_url: string | null;
  authenticated_at: number;
  is_default: boolean;
  github_domain: string;
}

export interface ManagedAuthStatus {
  provider: ManagedAuthProvider;
  authenticated: boolean;
  default_account_id: string | null;
  migration_error?: string | null;
  accounts: ManagedAuthAccount[];
}

export interface ManagedAuthDeviceCodeResponse {
  provider: ManagedAuthProvider;
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}

export async function authStartLogin(
  authProvider: ManagedAuthProvider,
  githubDomain?: string,
): Promise<ManagedAuthDeviceCodeResponse> {
  return invoke<ManagedAuthDeviceCodeResponse>("auth_start_login", {
    authProvider,
    githubDomain: githubDomain || null,
  });
}

export async function authPollForAccount(
  authProvider: ManagedAuthProvider,
  deviceCode: string,
  githubDomain?: string,
): Promise<ManagedAuthAccount | null> {
  return invoke<ManagedAuthAccount | null>("auth_poll_for_account", {
    authProvider,
    deviceCode,
    githubDomain: githubDomain || null,
  });
}

export async function authListAccounts(
  authProvider: ManagedAuthProvider,
): Promise<ManagedAuthAccount[]> {
  return invoke<ManagedAuthAccount[]>("auth_list_accounts", {
    authProvider,
  });
}

export async function authGetStatus(
  authProvider: ManagedAuthProvider,
): Promise<ManagedAuthStatus> {
  return invoke<ManagedAuthStatus>("auth_get_status", {
    authProvider,
  });
}

export async function authRemoveAccount(
  authProvider: ManagedAuthProvider,
  accountId: string,
): Promise<void> {
  return invoke("auth_remove_account", {
    authProvider,
    accountId,
  });
}

export async function authSetDefaultAccount(
  authProvider: ManagedAuthProvider,
  accountId: string,
): Promise<void> {
  return invoke("auth_set_default_account", {
    authProvider,
    accountId,
  });
}

export async function authLogout(
  authProvider: ManagedAuthProvider,
): Promise<void> {
  return invoke("auth_logout", {
    authProvider,
  });
}

export async function deepseekAccountAdd(params: {
  email?: string | null;
  mobile?: string | null;
  password: string;
}): Promise<DeepSeekAccount> {
  return invoke<DeepSeekAccount>("deepseek_account_add", {
    email: params.email || null,
    mobile: params.mobile || null,
    password: params.password,
  });
}

export async function deepseekAccountList(): Promise<DeepSeekAccount[]> {
  return invoke<DeepSeekAccount[]>("deepseek_account_list");
}

export async function deepseekAccountStatus(): Promise<DeepSeekAccountStatus> {
  return invoke<DeepSeekAccountStatus>("deepseek_account_status");
}

export async function deepseekAccountRemove(accountId: string): Promise<void> {
  return invoke("deepseek_account_remove", { accountId });
}

export async function deepseekAccountSetDefault(
  accountId: string,
): Promise<void> {
  return invoke("deepseek_account_set_default", { accountId });
}

export const authApi = {
  authStartLogin,
  authPollForAccount,
  authListAccounts,
  authGetStatus,
  authRemoveAccount,
  authSetDefaultAccount,
  authLogout,
  deepseekAccountAdd,
  deepseekAccountList,
  deepseekAccountStatus,
  deepseekAccountRemove,
  deepseekAccountSetDefault,
};
