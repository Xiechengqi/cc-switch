import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ExternalLink,
  Eye,
  EyeOff,
  MoreHorizontal,
  Play,
  Power,
  RefreshCw,
} from "lucide-react";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { useProxyTakeoverStatus } from "@/lib/query/proxy";
import { ShareDisplayStatusBadge } from "./ShareDisplayStatusBadge";
import { ShareRequestLogTable } from "./ShareRequestLogTable";
import {
  formatShareTokenUsage,
  formatUtcDateTime,
  getShareDisplayStatus,
  getShareUsageRatio,
  isPermanentExpiry,
  isShareActionAllowed,
  isUnlimitedTokenLimit,
  maskSensitive,
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
  const [revealKey, setRevealKey] = useState(false);
  const ratio = getShareUsageRatio(share);
  const isBusy = pendingAction === share.id;
  const tunnelDisplay = resolveShareTunnelInfo(share, tunnelConfig);
  const displayStatus = getShareDisplayStatus(
    share,
    tunnelConfigured,
    tunnelStatus,
  );
  const { data: takeoverStatus } = useProxyTakeoverStatus();

  const canDisable = isShareActionAllowed(
    share,
    "disable",
    tunnelConfigured,
    tunnelStatus,
  );
  const canEnable = isShareActionAllowed(
    share,
    "enable",
    tunnelConfigured,
    tunnelStatus,
  );
  const isFree = share.forSale === "Free";
  const apiKeyDisplay =
    revealKey || isFree ? share.shareToken : maskSensitive(share.shareToken);

  return (
    <Card className="border-border-default/70 bg-card/90">
      <CardContent className="space-y-5 px-5 py-5">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="text-lg font-semibold">{share.name}</h3>
              <ShareDisplayStatusBadge status={displayStatus} />
              {(["claude", "codex", "gemini"] as const).map((app) => {
                const active = takeoverStatus?.[app] ?? false;
                return (
                  <Badge
                    key={app}
                    variant="outline"
                    className={cn(
                      "rounded-full px-2.5 py-1 text-[11px] font-medium capitalize",
                      active
                        ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
                        : "border-muted bg-muted/50 text-muted-foreground",
                    )}
                  >
                    {app.charAt(0).toUpperCase() + app.slice(1)}
                  </Badge>
                );
              })}
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
              <span>
                {t("share.requestsCount")}: {share.requestsCount}
              </span>
              <span>
                {t("share.expiresAt")}:{" "}
                {isPermanentExpiry(share.expiresAt)
                  ? t("share.expiry.permanentLabel")
                  : formatUtcDateTime(share.expiresAt)}
              </span>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy}
              onClick={() => onRefresh?.(share)}
            >
              <RefreshCw className="h-4 w-4" />
              {t("share.refreshNow")}
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm" disabled={isBusy}>
                  <MoreHorizontal className="h-4 w-4" />
                  {t("share.more")}
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => onOpenConnect(share)}>
                  {t("share.connectInfo")}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => onOpenDetail(share)}>
                  {t("share.edit")}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  className="text-destructive focus:text-destructive"
                  onClick={() => onDelete(share)}
                >
                  {t("share.delete")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>

        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs text-muted-foreground">
              {t("share.tokensUsed")}
            </div>
            <div className="mt-2 font-medium">
              {formatShareTokenUsage(share)}
            </div>
            {!isUnlimitedTokenLimit(share.tokenLimit) ? (
              <div className="mt-3 h-2 rounded-full bg-muted">
                <div
                  className="h-2 rounded-full bg-blue-500"
                  style={{ width: `${Math.max(4, ratio * 100)}%` }}
                />
              </div>
            ) : null}
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs text-muted-foreground">
              {t("share.apiUrl")}
            </div>
            <div className="mt-2 break-all text-sm">
              {tunnelDisplay.tunnelUrl || "-"}
            </div>
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="flex items-center justify-between gap-2">
              <div className="text-xs text-muted-foreground">
                {t("share.apiKey")}
              </div>
              {!isFree ? (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  onClick={() => setRevealKey((prev) => !prev)}
                >
                  {revealKey ? (
                    <EyeOff className="h-3.5 w-3.5" />
                  ) : (
                    <Eye className="h-3.5 w-3.5" />
                  )}
                </Button>
              ) : null}
            </div>
            <div className="mt-2 break-all text-sm">{apiKeyDisplay || "-"}</div>
          </div>
          <div className="rounded-xl border border-border-default bg-muted/20 px-3 py-3">
            <div className="text-xs text-muted-foreground">
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
          {canDisable ? (
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy}
              onClick={() => onDisable(share)}
            >
              <Power className="h-4 w-4" />
              {t("share.disable")}
            </Button>
          ) : (
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy || !canEnable}
              onClick={() => onEnable(share)}
            >
              <Play className="h-4 w-4" />
              {t("share.enable")}
            </Button>
          )}
          {tunnelDisplay.tunnelUrl ? (
            <Button
              variant="ghost"
              size="sm"
              asChild
              className="text-blue-500 hover:text-blue-600"
            >
              <a
                href={tunnelDisplay.tunnelUrl}
                target="_blank"
                rel="noreferrer"
              >
                <ExternalLink className="h-4 w-4" />
                {t("share.openUrl")}
              </a>
            </Button>
          ) : null}
        </div>

        <ShareRequestLogTable shareId={share.id} />
      </CardContent>
    </Card>
  );
}
