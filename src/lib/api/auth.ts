import { invokeCommand, isTauriRuntime } from "@/lib/runtime";

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
  email?: string | null;
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

const LOCAL_CALLBACK_AUTH_PROVIDERS = new Set<ManagedAuthProvider>([
  "claude_oauth",
  "google_gemini_oauth",
  "antigravity_oauth",
]);

function isLoopbackHostname(hostname: string): boolean {
  const value = hostname.trim().toLowerCase();
  return (
    value === "localhost" ||
    value.endsWith(".localhost") ||
    value === "127.0.0.1" ||
    value === "0.0.0.0" ||
    value === "::1" ||
    value === "[::1]"
  );
}

export function isLocalCallbackAuthProvider(
  authProvider: ManagedAuthProvider,
): boolean {
  return LOCAL_CALLBACK_AUTH_PROVIDERS.has(authProvider);
}

export function shouldBlockLocalCallbackAuthInClientWeb(
  authProvider: ManagedAuthProvider,
): boolean {
  if (!isLocalCallbackAuthProvider(authProvider) || isTauriRuntime()) {
    return false;
  }
  if (typeof window === "undefined") {
    return false;
  }
  return !isLoopbackHostname(window.location.hostname);
}

export function localCallbackAuthBlockedMessage(): string {
  return "当前通过 client URL 访问，无法添加需要 localhost 回调的 OAuth 账号。请在 cc-switch 桌面端本机添加该账号后再回到 client URL 使用。Codex/Copilot/Kiro/Cursor 等非 localhost 回调登录不受影响。";
}

export async function authStartLogin(
  authProvider: ManagedAuthProvider,
  githubDomain?: string,
): Promise<ManagedAuthDeviceCodeResponse> {
  if (shouldBlockLocalCallbackAuthInClientWeb(authProvider)) {
    throw new Error(localCallbackAuthBlockedMessage());
  }
  return invokeCommand<ManagedAuthDeviceCodeResponse>("auth_start_login", {
    authProvider,
    githubDomain: githubDomain || null,
  });
}

export async function authPollForAccount(
  authProvider: ManagedAuthProvider,
  deviceCode: string,
  githubDomain?: string,
): Promise<ManagedAuthAccount | null> {
  return invokeCommand<ManagedAuthAccount | null>("auth_poll_for_account", {
    authProvider,
    deviceCode,
    githubDomain: githubDomain || null,
  });
}

export async function authListAccounts(
  authProvider: ManagedAuthProvider,
): Promise<ManagedAuthAccount[]> {
  return invokeCommand<ManagedAuthAccount[]>("auth_list_accounts", {
    authProvider,
  });
}

export async function authGetStatus(
  authProvider: ManagedAuthProvider,
): Promise<ManagedAuthStatus> {
  return invokeCommand<ManagedAuthStatus>("auth_get_status", {
    authProvider,
  });
}

export async function authRemoveAccount(
  authProvider: ManagedAuthProvider,
  accountId: string,
): Promise<void> {
  return invokeCommand("auth_remove_account", {
    authProvider,
    accountId,
  });
}

export async function authSetDefaultAccount(
  authProvider: ManagedAuthProvider,
  accountId: string,
): Promise<void> {
  return invokeCommand("auth_set_default_account", {
    authProvider,
    accountId,
  });
}

export async function authLogout(
  authProvider: ManagedAuthProvider,
): Promise<void> {
  return invokeCommand("auth_logout", {
    authProvider,
  });
}

export async function deepseekAccountAdd(params: {
  email?: string | null;
  mobile?: string | null;
  password: string;
}): Promise<DeepSeekAccount> {
  return invokeCommand<DeepSeekAccount>("deepseek_account_add", {
    email: params.email || null,
    mobile: params.mobile || null,
    password: params.password,
  });
}

export async function deepseekAccountList(): Promise<DeepSeekAccount[]> {
  return invokeCommand<DeepSeekAccount[]>("deepseek_account_list");
}

export async function deepseekAccountStatus(): Promise<DeepSeekAccountStatus> {
  return invokeCommand<DeepSeekAccountStatus>("deepseek_account_status");
}

export async function deepseekAccountRemove(accountId: string): Promise<void> {
  return invokeCommand("deepseek_account_remove", { accountId });
}

export async function deepseekAccountSetDefault(
  accountId: string,
): Promise<void> {
  return invokeCommand("deepseek_account_set_default", { accountId });
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
