import { useQuery } from "@tanstack/react-query";
import { copilotGetUsage, copilotGetUsageForAccount } from "@/lib/api/copilot";
import type { QuotaTier } from "@/types/subscription";
import { useSettingsQuery } from "./queries";
import { getOauthQuotaRefreshIntervalMs } from "./oauthQuotaRefresh";

export interface CopilotQuota {
  success: boolean;
  plan: string | null;
  resetDate: string | null;
  tiers: QuotaTier[];
  error: string | null;
  queriedAt: number | null;
}

export interface UseCopilotQuotaOptions {
  enabled?: boolean;
  /** 是否启用自动轮询与窗口 focus 重取，间隔由认证页统一配置 */
  autoQuery?: boolean;
}

export function useCopilotQuota(
  accountId: string | null,
  options: UseCopilotQuotaOptions = {},
) {
  const { enabled = true, autoQuery = false } = options;
  const { data: settings } = useSettingsQuery();
  const refreshInterval = getOauthQuotaRefreshIntervalMs(settings);
  return useQuery<CopilotQuota>({
    queryKey: ["copilot", "quota", accountId ?? "default"],
    queryFn: async (): Promise<CopilotQuota> => {
      const usage = accountId
        ? await copilotGetUsageForAccount(accountId)
        : await copilotGetUsage();

      const premium = usage.quota_snapshots.premium_interactions;
      const utilization =
        premium.entitlement > 0
          ? ((premium.entitlement - premium.remaining) / premium.entitlement) *
            100
          : 0;

      return {
        success: true,
        plan: usage.copilot_plan,
        resetDate: usage.quota_reset_date,
        tiers: [
          {
            name: "premium",
            utilization,
            resetsAt: usage.quota_reset_date,
          },
        ],
        error: null,
        queriedAt: Date.now(),
      };
    },
    enabled,
    refetchInterval: autoQuery ? refreshInterval : false,
    refetchIntervalInBackground: autoQuery,
    refetchOnWindowFocus: autoQuery,
    staleTime: refreshInterval,
    retry: 1,
  });
}
