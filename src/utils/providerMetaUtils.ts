import type { AppId } from "@/lib/api/types";
import { PROVIDER_TYPES } from "@/config/constants";
import type { CustomEndpoint, Provider, ProviderMeta } from "@/types";

/**
 * 合并供应商元数据中的自定义端点。
 * - 当 customEndpoints 为空对象时，明确删除自定义端点但保留其它元数据。
 * - 当 customEndpoints 为 null/undefined 时，不修改端点（保留原有端点）。
 * - 当 customEndpoints 存在时，覆盖原有自定义端点。
 * - 若结果为空对象且非明确清空场景则返回 undefined，避免写入空 meta。
 */
export function mergeProviderMeta(
  initialMeta: ProviderMeta | undefined,
  customEndpoints: Record<string, CustomEndpoint> | null | undefined,
): ProviderMeta | undefined {
  const hasCustomEndpoints =
    !!customEndpoints && Object.keys(customEndpoints).length > 0;

  // 明确清空：传入空对象（非 null/undefined）表示用户想要删除所有端点
  const isExplicitClear =
    customEndpoints !== null &&
    customEndpoints !== undefined &&
    Object.keys(customEndpoints).length === 0;

  if (hasCustomEndpoints) {
    return {
      ...(initialMeta ? { ...initialMeta } : {}),
      custom_endpoints: customEndpoints!,
    };
  }

  // 明确清空端点
  if (isExplicitClear) {
    if (!initialMeta) {
      // 新供应商且用户没有添加端点（理论上不会到这里）
      return undefined;
    }

    if ("custom_endpoints" in initialMeta) {
      const { custom_endpoints, ...rest } = initialMeta;
      // 保留其他字段（如 usage_script）
      // 即使 rest 为空，也要返回空对象（让后端知道要清空 meta）
      return Object.keys(rest).length > 0 ? rest : {};
    }

    // initialMeta 中本来就没有 custom_endpoints
    return { ...initialMeta };
  }

  // null/undefined：用户没有修改端点，保持不变
  if (!initialMeta) {
    return undefined;
  }

  if ("custom_endpoints" in initialMeta) {
    const { custom_endpoints, ...rest } = initialMeta;
    return Object.keys(rest).length > 0 ? rest : undefined;
  }

  return { ...initialMeta };
}

export function hasManagedAuthBinding(
  meta: ProviderMeta | undefined,
  authProvider: string,
): boolean {
  const binding = meta?.authBinding;
  return (
    binding?.source === "managed_account" &&
    binding.authProvider === authProvider &&
    typeof binding.accountId === "string" &&
    binding.accountId.trim() !== ""
  );
}

export function isCodexOfficialWithManagedAuth(
  provider: Pick<Provider, "category" | "meta">,
): boolean {
  return (
    provider.category === "official" &&
    hasManagedAuthBinding(provider.meta, "codex_oauth")
  );
}

export function isManagedOauthProvider(
  provider: Pick<Provider, "category" | "meta">,
  appId: AppId,
): boolean {
  return (
    provider.meta?.providerType === PROVIDER_TYPES.GITHUB_COPILOT ||
    provider.meta?.providerType === PROVIDER_TYPES.CODEX_OAUTH ||
    provider.meta?.providerType === PROVIDER_TYPES.CLAUDE_OAUTH ||
    provider.meta?.providerType === PROVIDER_TYPES.GOOGLE_GEMINI_OAUTH ||
    (appId === "codex" && isCodexOfficialWithManagedAuth(provider))
  );
}

export function isOfficialBlockedByProxyTakeover(
  provider: Pick<Provider, "category" | "meta">,
  appId: AppId,
  isProxyTakeover: boolean,
): boolean {
  return (
    isProxyTakeover &&
    provider.category === "official" &&
    !isManagedOauthProvider(provider, appId)
  );
}

export function canTestProvider(
  provider: Pick<Provider, "category" | "meta">,
  appId: AppId,
): boolean {
  if (provider.meta?.providerType === PROVIDER_TYPES.CLAUDE_OAUTH) {
    return true;
  }

  if (appId === "codex" && isCodexOfficialWithManagedAuth(provider)) {
    return true;
  }

  if (provider.category === "official") {
    return false;
  }

  if (
    provider.meta?.providerType === PROVIDER_TYPES.GITHUB_COPILOT ||
    provider.meta?.providerType === PROVIDER_TYPES.CODEX_OAUTH
  ) {
    return false;
  }

  return true;
}

export type ProviderQuotaSource =
  | "copilot"
  | "codex_oauth"
  | "claude_oauth"
  | "google_gemini_oauth"
  | "official"
  | "none";

export function getProviderQuotaSource(
  provider: Pick<Provider, "category" | "meta">,
  appId: AppId,
): ProviderQuotaSource {
  if (provider.meta?.providerType === PROVIDER_TYPES.GITHUB_COPILOT) {
    return "copilot";
  }

  if (provider.meta?.usage_script?.templateType === "github_copilot") {
    return "copilot";
  }

  if (provider.meta?.providerType === PROVIDER_TYPES.CLAUDE_OAUTH) {
    return "claude_oauth";
  }

  if (
    provider.meta?.providerType === PROVIDER_TYPES.CODEX_OAUTH ||
    (appId === "codex" && isCodexOfficialWithManagedAuth(provider))
  ) {
    return "codex_oauth";
  }

  if (provider.meta?.providerType === PROVIDER_TYPES.GOOGLE_GEMINI_OAUTH) {
    return "google_gemini_oauth";
  }

  if (provider.category === "official") {
    return "official";
  }

  return "none";
}
