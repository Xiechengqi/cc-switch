import { useQuery } from "@tanstack/react-query";
import { subscriptionApi } from "@/lib/api/subscription";
import type { AppId } from "@/lib/api/types";
import type { ProviderMeta } from "@/types";
import { resolveManagedAccountId } from "@/lib/authBinding";
import { PROVIDER_TYPES } from "@/config/constants";
import { useSettingsQuery } from "./queries";
import { getOauthQuotaRefreshIntervalMs } from "./oauthQuotaRefresh";

export const subscriptionKeys = {
  all: ["subscription"] as const,
  quota: (appId: AppId) => [...subscriptionKeys.all, "quota", appId] as const,
};

export function useSubscriptionQuota(
  appId: AppId,
  enabled: boolean,
  autoQuery = false,
) {
  const { data: settings } = useSettingsQuery();
  const refreshInterval = getOauthQuotaRefreshIntervalMs(settings);

  return useQuery({
    queryKey: subscriptionKeys.quota(appId),
    queryFn: () => subscriptionApi.getQuota(appId),
    enabled: enabled && ["claude", "codex", "gemini"].includes(appId),
    refetchInterval: autoQuery ? refreshInterval : false,
    refetchIntervalInBackground: autoQuery,
    refetchOnWindowFocus: autoQuery,
    staleTime: refreshInterval,
    retry: 1,
  });
}

export interface UseCodexOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
}

export interface UseClaudeOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
}

export interface UseGeminiOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
}

export function useClaudeOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseClaudeOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(meta, PROVIDER_TYPES.CLAUDE_OAUTH);
  return useQuery({
    queryKey: ["claude_oauth", "quota", accountId ?? "default"],
    queryFn: async () =>
      (await subscriptionApi.getCachedOauthQuota("claude_oauth", accountId))
        ?.quota,
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}

/**
 * Codex OAuth (ChatGPT Plus/Pro 反代) 订阅额度查询 hook
 *
 * 与 `useSubscriptionQuota` 平行：数据走 cc-switch 自管的 OAuth token，
 * 而不是 Codex CLI 的 ~/.codex/auth.json。
 *
 * Query key 包含 accountId，多张卡片绑定到同一账号时会自动去重共享请求。
 * accountId 为 null 时使用 "default" 占位，让后端 fallback 到默认账号。
 */
export function useCodexOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseCodexOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(meta, PROVIDER_TYPES.CODEX_OAUTH);
  return useQuery({
    queryKey: ["codex_oauth", "quota", accountId ?? "default"],
    queryFn: async () =>
      (await subscriptionApi.getCachedOauthQuota("codex_oauth", accountId))
        ?.quota,
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}

export function useGeminiOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseGeminiOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(
    meta,
    PROVIDER_TYPES.GOOGLE_GEMINI_OAUTH,
  );
  return useQuery({
    queryKey: ["google_gemini_oauth", "quota", accountId ?? "default"],
    queryFn: async () =>
      (
        await subscriptionApi.getCachedOauthQuota(
          "google_gemini_oauth",
          accountId,
        )
      )?.quota,
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}
