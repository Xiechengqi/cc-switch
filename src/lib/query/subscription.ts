import { useRef } from "react";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { subscriptionApi } from "@/lib/api/subscription";
import type { AppId } from "@/lib/api/types";
import type { ProviderMeta } from "@/types";
import type { SubscriptionQuota } from "@/types/subscription";
import { resolveManagedAccountId } from "@/lib/authBinding";
import { PROVIDER_TYPES } from "@/config/constants";
import { resolveDisplayUsage, type LastGoodSnapshot } from "./queries";
import { extractErrorMessage } from "@/utils/errorUtils";

const REFETCH_INTERVAL = 5 * 60 * 1000; // 5 minutes

export const subscriptionKeys = {
  all: ["subscription"] as const,
  quota: (appId: AppId) => [...subscriptionKeys.all, "quota", appId] as const,
};

/**
 * 读取缓存的 OAuth 用量；若缓存未命中（后台刷新尚未覆盖该 provider），
 * 主动触发一次强制刷新拉取新数据。后台事件仍是主刷新通道，此处仅兜底首次加载。
 */
async function fetchOauthQuotaWithFallback(
  authProvider: string,
  accountId: string | null,
  providerType?: string | null,
  appId?: AppId | null,
  providerId?: string | null,
) {
  const cached = await subscriptionApi.getCachedOauthQuota(
    authProvider,
    accountId,
    appId,
    providerId,
  );
  if (cached?.quota) return cached.quota;
  const refreshed = await subscriptionApi.refreshOauthQuota(
    authProvider,
    accountId,
    providerType,
    appId,
    providerId,
  );
  return refreshed?.quota;
}

/**
 * reject 且无可展示值时的失败占位：首次查询就失败（data 为 undefined），或
 * react-query 保留的旧成功已超出 keep-last-good 窗口——合成一个失败结果，让
 * 订阅视图仍渲染「查询失败」+ 刷新按钮，而不是 footer 整体消失、无从手动重查。
 */
const QUERY_REJECTED_PLACEHOLDER: SubscriptionQuota = {
  tool: "",
  credentialStatus: "valid",
  credentialMessage: null,
  success: false,
  tiers: [],
  extraUsage: null,
  error: null,
  queriedAt: null,
};

/**
 * Keep-last-good：与 useUsageQuery 同一策略（resolveDisplayUsage）。
 *
 * 后端对纯传输失败（网络/超时/读体中断）已 reject——react-query 保留上次 data
 * 并触发 retry，但那份 data 是陈旧的：以 `rejected` 标志交给 resolveDisplayUsage
 * 按同一窗口处理（窗口内继续展示，超窗透出失败），与仍以 `Ok(success:false)`
 * 返回的瞬时失败（HTTP 5xx/429）行为一致。确定性失败（过期/鉴权/解析）不掩盖，
 * 立即透出。
 *
 * `scopeKey` 标识查询身份（appId / 绑定的账号 id）：身份变化时丢弃旧快照，
 * 避免用上一个账号的额度掩盖新账号的瞬时失败。
 */
function useQuotaKeepLastGood(
  query: UseQueryResult<SubscriptionQuota>,
  scopeKey: string,
) {
  const lastGoodRef = useRef<{
    key: string;
    snap: LastGoodSnapshot<SubscriptionQuota> | null;
  }>({ key: scopeKey, snap: null });
  if (lastGoodRef.current.key !== scopeKey) {
    lastGoodRef.current = { key: scopeKey, snap: null };
  }
  const { data, lastGood } = resolveDisplayUsage(
    query.data,
    query.dataUpdatedAt,
    lastGoodRef.current.snap,
    Date.now(),
    { rejected: query.isError },
  );
  lastGoodRef.current.snap = lastGood;
  return {
    ...query,
    data:
      data ??
      (query.isError
        ? {
            ...QUERY_REJECTED_PLACEHOLDER,
            error: extractErrorMessage(query.error) || null,
          }
        : undefined),
  };
}

export function useSubscriptionQuota(
  appId: AppId,
  enabled: boolean,
  autoQuery = false,
  autoQueryIntervalMinutes = 5,
) {
  const refetchInterval =
    autoQuery && autoQueryIntervalMinutes > 0
      ? Math.max(autoQueryIntervalMinutes, 1) * 60 * 1000
      : false;

  const query = useQuery({
    queryKey: subscriptionKeys.quota(appId),
    queryFn: () => subscriptionApi.getQuota(appId),
    enabled: enabled && ["claude", "codex", "gemini"].includes(appId),
    refetchInterval,
    refetchIntervalInBackground: Boolean(refetchInterval),
    refetchOnWindowFocus: Boolean(refetchInterval),
    staleTime:
      autoQueryIntervalMinutes > 0
        ? Math.max(autoQueryIntervalMinutes, 1) * 60 * 1000
        : REFETCH_INTERVAL,
    retry: 1,
  });

  return useQuotaKeepLastGood(query, appId);
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

export interface UseKiroOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
}

export function resolveCodexQuotaAuthProvider(): string {
  return PROVIDER_TYPES.CODEX_OAUTH;
}

export function useClaudeOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseClaudeOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(meta, PROVIDER_TYPES.CLAUDE_OAUTH);
  return useQuery({
    queryKey: ["claude_oauth", "quota", accountId ?? "default"],
    queryFn: async () => fetchOauthQuotaWithFallback("claude_oauth", accountId),
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
  const authProvider = resolveCodexQuotaAuthProvider();
  const accountId = resolveManagedAccountId(meta, authProvider);
  return useQuery({
    queryKey: [authProvider, "quota", accountId ?? "default"],
    queryFn: async () =>
      fetchOauthQuotaWithFallback(authProvider, accountId, meta?.providerType),
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
      fetchOauthQuotaWithFallback("google_gemini_oauth", accountId),
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}

export function useKiroOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseKiroOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(meta, PROVIDER_TYPES.KIRO_OAUTH);
  return useQuery({
    queryKey: ["kiro_oauth", "quota", accountId ?? "default"],
    queryFn: async () => fetchOauthQuotaWithFallback("kiro_oauth", accountId),
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}

export interface UseAntigravityOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
}

export interface UseCursorOauthQuotaOptions {
  enabled?: boolean;
  autoQuery?: boolean;
  appId?: AppId;
  providerId?: string;
}

export function useAntigravityOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseAntigravityOauthQuotaOptions = {},
) {
  const { enabled = true } = options;
  const accountId = resolveManagedAccountId(
    meta,
    PROVIDER_TYPES.ANTIGRAVITY_OAUTH,
  );
  return useQuery({
    queryKey: ["antigravity_oauth", "quota", accountId ?? "default"],
    queryFn: async () =>
      fetchOauthQuotaWithFallback(
        "antigravity_oauth",
        accountId,
        meta?.providerType,
      ),
    enabled,
    refetchInterval: false,
    refetchOnWindowFocus: false,
    staleTime: Infinity,
    retry: false,
  });
}

export function useCursorOauthQuota(
  meta: ProviderMeta | undefined,
  options: UseCursorOauthQuotaOptions = {},
) {
  const { enabled = true, appId, providerId } = options;
  const isCursorApiKey = meta?.providerType === PROVIDER_TYPES.CURSOR_APIKEY;
  const authProvider = isCursorApiKey
    ? PROVIDER_TYPES.CURSOR_APIKEY
    : PROVIDER_TYPES.CURSOR_OAUTH;
  const accountId = isCursorApiKey
    ? null
    : resolveManagedAccountId(meta, PROVIDER_TYPES.CURSOR_OAUTH);
  return useQuery({
    queryKey: [
      authProvider,
      "quota",
      accountId ?? providerId ?? "default",
      appId ?? "unknown",
    ],
    queryFn: async () =>
      fetchOauthQuotaWithFallback(
        authProvider,
        accountId,
        meta?.providerType,
        appId,
        providerId,
      ),
    enabled: enabled && (!isCursorApiKey || Boolean(appId && providerId)),
    refetchInterval: false,
    refetchOnWindowFocus: false,
    refetchOnMount: isCursorApiKey ? "always" : true,
    staleTime: isCursorApiKey ? 30 * 1000 : Infinity,
    retry: isCursorApiKey ? 1 : false,
  });
}
