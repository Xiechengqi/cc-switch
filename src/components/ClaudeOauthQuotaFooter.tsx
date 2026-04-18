import React from "react";
import type { ProviderMeta } from "@/types";
import { useClaudeOauthQuota } from "@/lib/query/subscription";
import { SubscriptionQuotaView } from "@/components/SubscriptionQuotaFooter";

interface ClaudeOauthQuotaFooterProps {
  meta?: ProviderMeta;
  inline?: boolean;
  /** 是否为当前激活的供应商 */
  isCurrent?: boolean;
}

const ClaudeOauthQuotaFooter: React.FC<ClaudeOauthQuotaFooterProps> = ({
  meta,
  inline = false,
  isCurrent = false,
}) => {
  const {
    data: quota,
    isFetching: loading,
    refetch,
  } = useClaudeOauthQuota(meta, { enabled: true, autoQuery: isCurrent });

  return (
    <SubscriptionQuotaView
      quota={quota}
      loading={loading}
      refetch={refetch}
      appIdForExpiredHint="claude_oauth"
      inline={inline}
    />
  );
};

export default ClaudeOauthQuotaFooter;
