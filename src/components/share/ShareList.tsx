import { useTranslation } from "react-i18next";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { ShareCard } from "./ShareCard";

interface ShareListProps {
  shares: ShareRecord[];
  tunnelStatusMap: Record<string, TunnelInfo | null | undefined>;
  tunnelConfig: TunnelConfig;
  tunnelConfigured: boolean;
  isLoading: boolean;
  error: string | null;
  pendingAction?: string | null;
  onRetry: () => void;
  onCreate: () => void;
  onOpenDetail: (share: ShareRecord) => void;
  onOpenConnect: (share: ShareRecord) => void;
  onDelete: (share: ShareRecord) => void;
  onEnable: (share: ShareRecord) => void;
  onDisable: (share: ShareRecord) => void;
  onRefresh: (share: ShareRecord) => void;
}

export function ShareList({
  shares,
  tunnelStatusMap,
  tunnelConfig,
  tunnelConfigured,
  isLoading,
  error,
  pendingAction,
  onRetry,
  onCreate,
  onOpenDetail,
  onOpenConnect,
  onDelete,
  onEnable,
  onDisable,
  onRefresh,
}: ShareListProps) {
  const { t } = useTranslation();

  if (error) {
    return (
      <Card className="border-red-500/30 bg-red-500/5">
        <CardContent className="flex flex-col items-start gap-4 px-6 py-6">
          <div>
            <div className="text-base font-medium">{t("share.error.title")}</div>
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
          onOpenDetail={onOpenDetail}
          onOpenConnect={onOpenConnect}
          onDelete={onDelete}
          onEnable={onEnable}
          onDisable={onDisable}
          onRefresh={onRefresh}
        />
      ))}
    </div>
  );
}
