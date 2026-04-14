import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ExternalLink, Play, Power, Trash2, Info, Copy, RefreshCw } from "lucide-react";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { SHARE_REFRESH_INTERVAL_MS } from "@/lib/query/share";
import { ShareStatusBadge } from "./ShareStatusBadge";
import { ShareRequestLogTable } from "./ShareRequestLogTable";
import {
  formatUtcDateTime,
  getShareTunnelRuntimeStatus,
  getShareUsageRatio,
  isShareActionAllowed,
  resolveShareTunnelInfo,
} from "@/utils/shareUtils";

interface ShareCardProps {
  share: ShareRecord;
  tunnelStatus?: TunnelInfo | null;
  tunnelConfig: TunnelConfig;
  tunnelConfigured: boolean;
  pendingAction?: string | null;
  onOpenDetail: (share: ShareRecord) => void;
  onOpenConnect: (share: ShareRecord) => void;
  onDelete: (share: ShareRecord) => void;
  onEnable: (share: ShareRecord) => void;
  onDisable: (share: ShareRecord) => void;
  onRefresh?: (share: ShareRecord) => void;
}

export function ShareCard({
  share,
  tunnelStatus,
  tunnelConfig,
  tunnelConfigured,
  pendingAction,
  onOpenDetail,
  onOpenConnect,
  onDelete,
  onEnable,
  onDisable,
  onRefresh,
}: ShareCardProps) {
  const { t } = useTranslation();
  const ratio = getShareUsageRatio(share);
  const isBusy = pendingAction === share.id;
  const tunnelDisplay = resolveShareTunnelInfo(share, tunnelConfig);
  const tunnelRuntimeStatus = getShareTunnelRuntimeStatus(share, tunnelStatus);
  const [refreshCountdown, setRefreshCountdown] = useState(
    Math.ceil(SHARE_REFRESH_INTERVAL_MS / 1000),
  );

  useEffect(() => {
    setRefreshCountdown(Math.ceil(SHARE_REFRESH_INTERVAL_MS / 1000));
    const interval = window.setInterval(() => {
      setRefreshCountdown((current) =>
        current <= 1 ? Math.ceil(SHARE_REFRESH_INTERVAL_MS / 1000) : current - 1,
      );
    }, 1000);

    return () => window.clearInterval(interval);
  }, [share.id]);

  return (
    <Card className="border-border-default/70 bg-card/90">
      <CardContent className="space-y-5 px-5 py-5">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="text-lg font-semibold">{share.name}</h3>
              <ShareStatusBadge status={share.status} />
              <ShareStatusBadge kind="tunnel" status={tunnelRuntimeStatus} />
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
              <span>
                {t("share.requestsCount")}: {share.requestsCount}
              </span>
              <span>
                {t("share.expiresAt")}: {formatUtcDateTime(share.expiresAt)}
              </span>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <div className="text-xs text-muted-foreground">
              {t("share.refreshIn", { seconds: refreshCountdown })}
            </div>
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy}
              onClick={() => {
                setRefreshCountdown(Math.ceil(SHARE_REFRESH_INTERVAL_MS / 1000));
                onRefresh?.(share);
              }}
            >
              <RefreshCw className="h-4 w-4" />
              {t("share.refreshNow")}
            </Button>
            <Button variant="outline" size="sm" onClick={() => onOpenConnect(share)}>
              <Copy className="h-4 w-4" />
              {t("share.connectInfo")}
            </Button>
            <Button variant="outline" size="sm" onClick={() => onOpenDetail(share)}>
              <Info className="h-4 w-4" />
              {t("share.edit")}
            </Button>
          </div>
        </div>

        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
              {t("share.tokensUsed")}
            </div>
            <div className="mt-2 font-medium">
              {share.tokensUsed} / {share.tokenLimit}
            </div>
            <div className="mt-3 h-2 rounded-full bg-muted">
              <div
                className="h-2 rounded-full bg-blue-500"
                style={{ width: `${Math.max(4, ratio * 100)}%` }}
              />
            </div>
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
              {t("share.subdomain")}
            </div>
            <div className="mt-2 break-all text-sm">{tunnelDisplay.subdomain || "-"}</div>
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
              {t("share.tunnelUrl")}
            </div>
            <div className="mt-2 break-all text-sm">{tunnelDisplay.tunnelUrl || "-"}</div>
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
              {t("share.lastUsedAt")}
            </div>
            <div className="mt-2 text-sm">
              {share.lastUsedAt
                ? formatUtcDateTime(share.lastUsedAt)
                : t("share.never")}
            </div>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={
              isBusy ||
              !isShareActionAllowed(share, "enable", tunnelConfigured, tunnelStatus)
            }
            onClick={() => onEnable(share)}
          >
            <Play className="h-4 w-4" />
            {t("share.enable")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={
              isBusy ||
              !isShareActionAllowed(share, "disable", tunnelConfigured, tunnelStatus)
            }
            onClick={() => onDisable(share)}
          >
            <Power className="h-4 w-4" />
            {t("share.disable")}
          </Button>
          {tunnelDisplay.tunnelUrl ? (
            <Button
              variant="ghost"
              size="sm"
              asChild
              className="text-blue-500 hover:text-blue-600"
            >
              <a href={tunnelDisplay.tunnelUrl} target="_blank" rel="noreferrer">
                <ExternalLink className="h-4 w-4" />
                {t("share.openUrl")}
              </a>
            </Button>
          ) : null}
          <Button
            variant="destructive"
            size="sm"
            disabled={isBusy}
            onClick={() => onDelete(share)}
          >
            <Trash2 className="h-4 w-4" />
            {t("share.delete")}
          </Button>
        </div>

        <ShareRequestLogTable shareId={share.id} />
      </CardContent>
    </Card>
  );
}
