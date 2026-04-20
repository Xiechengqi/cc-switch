import { invoke } from "@tauri-apps/api/core";
import type { SubscriptionQuota } from "@/types/subscription";

export interface CachedOauthQuota {
  authProvider: string;
  accountId: string;
  providerId?: string | null;
  providerName?: string | null;
  appType?: string | null;
  quota: SubscriptionQuota;
  refreshedAt: number;
  nextRefreshAt?: number | null;
  source: string;
}

export const subscriptionApi = {
  getQuota: (tool: string): Promise<SubscriptionQuota> =>
    invoke("get_subscription_quota", { tool }),
  getClaudeOauthQuota: (accountId: string | null): Promise<SubscriptionQuota> =>
    invoke("get_claude_oauth_quota", { accountId }),
  getCodexOauthQuota: (accountId: string | null): Promise<SubscriptionQuota> =>
    invoke("get_codex_oauth_quota", { accountId }),
  getCachedOauthQuota: (
    authProvider: string,
    accountId: string | null,
  ): Promise<CachedOauthQuota | null> =>
    invoke("get_cached_oauth_quota", { authProvider, accountId }),
  getCodingPlanQuota: (
    baseUrl: string,
    apiKey: string,
  ): Promise<SubscriptionQuota> =>
    invoke("get_coding_plan_quota", { baseUrl, apiKey }),
  getBalance: (
    baseUrl: string,
    apiKey: string,
  ): Promise<import("@/types").UsageResult> =>
    invoke("get_balance", { baseUrl, apiKey }),
};
