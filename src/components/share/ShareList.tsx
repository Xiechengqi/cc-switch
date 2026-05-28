import { useTranslation } from "react-i18next";
import type {
  PublicMarket,
  ShareRecord,
  TunnelConfig,
  TunnelInfo,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { ShareCard, type ShareProviderSalePricing } from "./ShareCard";

interface ShareListProps {
  shares: ShareRecord[];
  tunnelStatusMap: Record<string, TunnelInfo | null | undefined>;
  tunnelConfig: TunnelConfig;
  tunnelConfigured: boolean;
  isLoading: boolean;
  error: string | null;
  pendingAction?: string | null;
  markets?: PublicMarket[];
  providerSalePricing?: ShareProviderSalePricing[];
  marketsLoading?: boolean;
  marketsError?: string | null;
  onRetryMarkets?: () => void;
  onRetry: () => void;
  onCreate: () => void;
  onDelete: (share: ShareRecord) => void;
  onEnable: (share: ShareRecord) => void;
  onDisable: (share: ShareRecord) => void;
  onResetUsage: (share: ShareRecord) => Promise<void> | void;
  onUpdateTokenLimit: (
    share: ShareRecord,
    tokenLimit: number,
  ) => Promise<void> | void;
  onUpdateParallelLimit: (
    share: ShareRecord,
    parallelLimit: number,
  ) => Promise<void> | void;
  onUpdateSubdomain: (
    share: ShareRecord,
    subdomain: string,
  ) => Promise<void> | void;
  onUpdateDescription: (
    share: ShareRecord,
    description: string,
  ) => Promise<void> | void;
  onUpdateForSale: (
    share: ShareRecord,
    forSale: "Yes" | "No" | "Free",
  ) => Promise<void> | void;
  onUpdateShareSalePricing: (
    share: ShareRecord,
    pricing: Record<string, number>,
  ) => Promise<void> | void;
  onUpdateExpiration: (
    share: ShareRecord,
    expiresAt: string,
  ) => Promise<void> | void;
  onUpdateAutoStart: (
    share: ShareRecord,
    autoStart: boolean,
  ) => Promise<void> | void;
  onUpdateOwnerEmail: (
    share: ShareRecord,
    ownerEmail: string,
  ) => Promise<void> | void;
  onUpdateAcl: (
    share: ShareRecord,
    sharedWithEmails: string[],
    marketAccessMode: "selected" | "all",
  ) => Promise<void> | void;
}

export function ShareList({
  shares,
  tunnelStatusMap,
  tunnelConfig,
  tunnelConfigured,
  isLoading,
  error,
  pendingAction,
  markets,
  providerSalePricing,
  marketsLoading,
  marketsError,
  onRetryMarkets,
  onRetry,
  onCreate,
  onDelete,
  onEnable,
  onDisable,
  onResetUsage,
  onUpdateTokenLimit,
  onUpdateParallelLimit,
  onUpdateSubdomain,
  onUpdateDescription,
  onUpdateForSale,
  onUpdateShareSalePricing,
  onUpdateExpiration,
  onUpdateAutoStart,
  onUpdateOwnerEmail,
  onUpdateAcl,
}: ShareListProps) {
  const { t } = useTranslation();

  if (error) {
    return (
      <Card className="border-red-500/30 bg-red-500/5">
        <CardContent className="flex flex-col items-start gap-4 px-6 py-6">
          <div>
            <div className="text-base font-medium">
              {t("share.error.title")}
            </div>
            <div className="mt-1 text-sm text-muted-foreground">{error}</div>
          </div>
          <Button variant="outline" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </CardContent>
      </Card>
    );
  }

  if (isLoading) {
    return (
      <div className="grid gap-4">
        {Array.from({ length: 3 }).map((_, index) => (
          <div
            key={index}
            className="h-52 animate-pulse rounded-2xl border border-border-default bg-muted/30"
          />
        ))}
      </div>
    );
  }

  if (shares.length === 0) {
    return (
      <Card className="border-dashed border-border-default/80 bg-muted/15">
        <CardContent className="flex flex-col items-center gap-4 px-6 py-12 text-center">
          <div className="space-y-2">
            <h3 className="text-xl font-semibold">{t("share.empty")}</h3>
            <p className="max-w-xl text-sm text-muted-foreground">
              {t("share.emptyDescription")}
            </p>
          </div>
          <Button onClick={onCreate}>{t("share.emptyCta")}</Button>
        </CardContent>
      </Card>
    );
  }

  return (
    <div className="grid gap-4">
      {shares.map((share) => (
        <ShareCard
          key={share.id}
          share={share}
          tunnelStatus={tunnelStatusMap[share.id]}
          tunnelConfig={tunnelConfig}
          tunnelConfigured={tunnelConfigured}
          pendingAction={pendingAction}
          markets={markets}
          providerSalePricing={providerSalePricing}
          marketsLoading={marketsLoading}
          marketsError={marketsError}
          onRetryMarkets={onRetryMarkets}
          onDelete={onDelete}
          onEnable={onEnable}
          onDisable={onDisable}
          onResetUsage={onResetUsage}
          onUpdateTokenLimit={onUpdateTokenLimit}
          onUpdateParallelLimit={onUpdateParallelLimit}
          onUpdateSubdomain={onUpdateSubdomain}
          onUpdateDescription={onUpdateDescription}
          onUpdateForSale={onUpdateForSale}
          onUpdateShareSalePricing={onUpdateShareSalePricing}
          onUpdateExpiration={onUpdateExpiration}
          onUpdateAutoStart={onUpdateAutoStart}
          onUpdateOwnerEmail={onUpdateOwnerEmail}
          onUpdateAcl={onUpdateAcl}
        />
      ))}
    </div>
  );
}
