import React from "react";
import type { ProviderMeta } from "@/types";
import { useGeminiOauthQuota } from "@/lib/query/subscription";
import type { AppId } from "@/lib/api";
import { SubscriptionQuotaView } from "@/components/SubscriptionQuotaFooter";

interface GeminiOauthQuotaFooterProps {
  meta?: ProviderMeta;
  appId?: AppId;
  providerId?: string;
  inline?: boolean;
  isCurrent?: boolean;
}

const GeminiOauthQuotaFooter: React.FC<GeminiOauthQuotaFooterProps> = ({
  meta,
  inline = false,
}) => {
  const {
    data: quota,
    isFetching: loading,
    refetch,
  } = useGeminiOauthQuota(meta, { enabled: true });
  const handleRefresh = React.useCallback(async () => {
    await refetch();
  }, [refetch]);

  return (
    <SubscriptionQuotaView
      quota={quota}
      loading={loading}
      refetch={handleRefresh}
      appIdForExpiredHint="google_gemini_oauth"
      inline={inline}
      visibleTierNames={["gemini_pro"]}
    />
  );
};

export default GeminiOauthQuotaFooter;
