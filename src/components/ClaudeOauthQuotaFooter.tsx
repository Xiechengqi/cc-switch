import React from "react";
import type { ProviderMeta } from "@/types";
import { useClaudeOauthQuota } from "@/lib/query/subscription";
import type { AppId } from "@/lib/api";
import { SubscriptionQuotaView } from "@/components/SubscriptionQuotaFooter";

interface ClaudeOauthQuotaFooterProps {
  meta?: ProviderMeta;
  appId?: AppId;
  providerId?: string;
  inline?: boolean;
  /** 是否为当前激活的供应商 */
  isCurrent?: boolean;
}

const ClaudeOauthQuotaFooter: React.FC<ClaudeOauthQuotaFooterProps> = ({
  meta,
  inline = false,
}) => {
  const {
    data: quota,
    isFetching: loading,
    refetch,
  } = useClaudeOauthQuota(meta, { enabled: true });
  const handleRefresh = React.useCallback(async () => {
    await refetch();
  }, [refetch]);

  return (
    <SubscriptionQuotaView
      quota={quota}
      loading={loading}
      refetch={handleRefresh}
      appIdForExpiredHint="claude_oauth"
      inline={inline}
    />
  );
};

export default ClaudeOauthQuotaFooter;
