import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Copy,
  Edit3,
  Play,
  Power,
  RotateCcw,
  Trash2,
} from "lucide-react";
import type {
  PublicMarket,
  ShareRecord,
  TunnelConfig,
  TunnelInfo,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { useProxyTakeoverStatus } from "@/lib/query/proxy";
import { copyText } from "@/lib/clipboard";
import { toast } from "sonner";
import { SHARE_REGIONS } from "@/config/shareRegions";
import { EditShareDialog } from "./EditShareDialog";
import { ShareDisplayStatusBadge } from "./ShareDisplayStatusBadge";
import { ShareRequestLogTable } from "./ShareRequestLogTable";
import {
  formatShareTokenUsage,
  formatUtcDateTime,
  getShareDisplayStatus,
  getShareTunnelRuntimeStatus,
  getShareUsageRatio,
  isPermanentExpiry,
  isShareActionAllowed,
  isUnlimitedParallelLimit,
  isUnlimitedTokenLimit,
  resolveShareTunnelInfo,
} from "@/utils/shareUtils";

export interface ShareProviderSalePricing {
  app: "claude" | "codex" | "gemini";
  label: string;
  providerName?: string;
  percent?: number;
}

interface ShareCardProps {
  share: ShareRecord;
  tunnelStatus?: TunnelInfo | null;
  tunnelConfig: TunnelConfig;
  tunnelConfigured: boolean;
  pendingAction?: string | null;
  markets?: PublicMarket[];
  providerSalePricing?: ShareProviderSalePricing[];
  marketsLoading?: boolean;
  marketsError?: string | null;
  onRetryMarkets?: () => void;
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
  onTransferOwner: (
    share: ShareRecord,
    targetEmail: string,
  ) => Promise<void> | void;
  onUpdateAcl: (
    share: ShareRecord,
    sharedWithEmails: string[],
    marketAccessMode: "selected" | "all",
  ) => Promise<void> | void;
}

const EMPTY_MARKETS: PublicMarket[] = [];
const EMPTY_PROVIDER_SALE_PRICING: ShareProviderSalePricing[] = [];

export function ShareCard({
  share,
  tunnelStatus,
  tunnelConfig,
  tunnelConfigured,
  pendingAction,
  markets = EMPTY_MARKETS,
  providerSalePricing = EMPTY_PROVIDER_SALE_PRICING,
  marketsLoading = false,
  marketsError = null,
  onRetryMarkets,
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
  onTransferOwner,
  onUpdateAcl,
}: ShareCardProps) {
  const { t } = useTranslation();
  const [editDialogOpen, setEditDialogOpen] = useState(false);
  const ratio = getShareUsageRatio(share);
  const isBusy = pendingAction === share.id;
  const tunnelDisplay = resolveShareTunnelInfo(share, tunnelConfig);
  const tunnelRuntimeStatus = getShareTunnelRuntimeStatus(share, tunnelStatus);
  const routerRegion = SHARE_REGIONS.find(
    (region) => region.baseUrl === tunnelConfig.domain,
  );
  const routerDisplay = routerRegion
    ? `${routerRegion.region} - ${routerRegion.baseUrl}`
    : tunnelConfig.domain;
  const displayStatus = getShareDisplayStatus(
    share,
    tunnelConfigured,
    tunnelStatus,
  );
  const { data: takeoverStatus } = useProxyTakeoverStatus();

  const marketEmailSet = new Set(
    markets.map((market) => market.email.toLowerCase()),
  );
  const currentNonMarketEmails = Array.from(
    new Set(
      (share.sharedWithEmails ?? [])
        .map((email) => email.trim().toLowerCase())
        .filter((email) => email && !marketEmailSet.has(email)),
    ),
  ).sort();
  const currentMarketEmails = Array.from(
    new Set(
      (share.sharedWithEmails ?? [])
        .map((email) => email.trim().toLowerCase())
        .filter((email) => marketEmailSet.has(email)),
    ),
  ).sort();
  const currentMarketAccessMode = share.marketAccessMode ?? "selected";

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
  const canDelete = share.status === "paused";

  const handleCopy = async (value: string, key: string) => {
    await copyText(value);
    toast.success(t(key));
  };

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
                {t("share.tokensUsed")}: {formatShareTokenUsage(share)}
              </span>
              <span>
                {t("share.expiresAt")}:{" "}
                {isPermanentExpiry(share.expiresAt)
                  ? t("share.expiry.permanentLabel")
                  : formatUtcDateTime(share.expiresAt)}
              </span>
              <span>
                {t("share.lastUsedAt")}:{" "}
                {share.lastUsedAt
                  ? formatUtcDateTime(share.lastUsedAt)
                  : t("share.never")}
              </span>
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
              <span>
                {t("share.id")}: {share.id}
              </span>
              <span>
                {t("share.createdAt")}: {formatUtcDateTime(share.createdAt)}
              </span>
              <span>
                {t("share.remotePort")}:{" "}
                {tunnelStatus?.remotePort
                  ? String(tunnelStatus.remotePort)
                  : "-"}
              </span>
              <span>
                {t("share.tunnelHealth")}:{" "}
                {t(`share.statuses.${tunnelRuntimeStatus}`)}
              </span>
              <span>
                {t("share.tunnel.region")}: {routerDisplay}
              </span>
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
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy}
              onClick={() => {
                if (!window.confirm(t("share.confirmResetUsageMessage"))) {
                  return;
                }
                void onResetUsage(share);
              }}
            >
              <RotateCcw className="h-4 w-4" />
              {t("share.resetUsage")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy}
              onClick={() => setEditDialogOpen(true)}
            >
              <Edit3 className="h-4 w-4" />
              {t("share.edit")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={isBusy || !canDelete}
              className="text-destructive hover:text-destructive"
              onClick={() => onDelete(share)}
            >
              <Trash2 className="h-4 w-4" />
              {t("share.delete")}
            </Button>
          </div>
        </div>

        <section className="space-y-3 border-t border-border-default/70 pt-4">
          <div className="text-sm font-semibold">{t("share.connectInfo")}</div>

          <div className="grid gap-2 lg:grid-cols-3">
            <ConnectInlineValue
              label={t("share.tunnelUrl")}
              value={tunnelDisplay.tunnelUrl}
              onCopy={() =>
                void handleCopy(tunnelDisplay.tunnelUrl, "share.toast.copyUrl")
              }
            />
            <ConnectInlineValue
              label={t("share.subdomain")}
              value={tunnelDisplay.subdomain}
              onCopy={() =>
                void handleCopy(
                  tunnelDisplay.subdomain,
                  "share.toast.copySubdomain",
                )
              }
            />
          </div>
        </section>

        <section className="space-y-4 border-t border-border-default/70 pt-4">
          <div className="text-sm font-semibold">
            {t("share.settings", { defaultValue: "设置项" })}
          </div>

          {!isUnlimitedTokenLimit(share.tokenLimit) ? (
            <div className="h-2 rounded-full bg-muted">
              <div
                className="h-2 rounded-full bg-blue-500"
                style={{ width: `${Math.max(4, ratio * 100)}%` }}
              />
            </div>
          ) : null}

          <div className="grid gap-2 md:grid-cols-3">
            <SummaryLine
              label={t("share.ownerEmail", { defaultValue: "Owner Email" })}
              value={share.ownerEmail || "-"}
            />
            <SummaryLine
              label={t("share.sharedWithEmails", { defaultValue: "Share To" })}
              value={
                currentNonMarketEmails.length
                  ? currentNonMarketEmails.join(", ")
                  : "-"
              }
            />
            <SummaryLine
              label={t("share.forSale")}
              value={t(`share.forSaleOptions.${share.forSale.toLowerCase()}`)}
            />
            <MarketSummary
              markets={markets}
              marketAccessMode={currentMarketAccessMode}
              selectedMarketEmails={currentMarketEmails}
            />
            <SummaryLine
              label={t("share.description")}
              value={share.description || "-"}
            />
            <SummaryLine
              label={t("share.tokenLimit")}
              value={
                isUnlimitedTokenLimit(share.tokenLimit)
                  ? t("share.unlimited")
                  : String(share.tokenLimit)
              }
            />
            <SummaryLine
              label={t("share.expiresAt")}
              value={
                isPermanentExpiry(share.expiresAt)
                  ? t("share.expiry.permanentLabel")
                  : formatUtcDateTime(share.expiresAt)
              }
            />
            <SummaryLine
              label={t("share.parallelLimit")}
              value={
                isUnlimitedParallelLimit(share.parallelLimit)
                  ? t("share.unlimited")
                  : String(share.parallelLimit)
              }
            />
            <SummaryLine
              label={t("share.autoStart")}
              value={
                share.autoStart ? t("common.enabled") : t("common.disabled")
              }
            />
          </div>
        </section>

        <ShareRequestLogTable shareId={share.id} />
      </CardContent>

      <EditShareDialog
        open={editDialogOpen}
        onOpenChange={setEditDialogOpen}
        share={share}
        markets={markets}
        providerSalePricing={providerSalePricing}
        marketsLoading={marketsLoading}
        marketsError={marketsError}
        onRetryMarkets={onRetryMarkets}
        isBusy={isBusy}
        onUpdateTokenLimit={onUpdateTokenLimit}
        onUpdateParallelLimit={onUpdateParallelLimit}
        onUpdateSubdomain={onUpdateSubdomain}
        onUpdateDescription={onUpdateDescription}
        onUpdateForSale={onUpdateForSale}
        onUpdateShareSalePricing={onUpdateShareSalePricing}
        onUpdateExpiration={onUpdateExpiration}
        onUpdateAutoStart={onUpdateAutoStart}
        onUpdateOwnerEmail={onUpdateOwnerEmail}
        onTransferOwner={onTransferOwner}
        onUpdateAcl={onUpdateAcl}
      />
    </Card>
  );
}

function MarketSummary({
  markets,
  marketAccessMode,
  selectedMarketEmails,
}: {
  markets: PublicMarket[];
  marketAccessMode: "selected" | "all";
  selectedMarketEmails: string[];
}) {
  const { t } = useTranslation();
  const marketByEmail = new Map(
    markets.map((market) => [market.email.toLowerCase(), market]),
  );

  return (
    <div className="min-w-0 rounded-md border border-border-default/70 bg-muted/10 px-3 py-2">
      <div className="text-xs text-muted-foreground">
        {t("share.market.title", { defaultValue: "Market" })}
      </div>
      <div className="mt-2">
        {marketAccessMode === "all" ? (
          <div className="text-sm text-muted-foreground">
            {t("share.market.allSelected", {
              defaultValue: "已选中所有 Market",
            })}
          </div>
        ) : selectedMarketEmails.length ? (
          <div className="flex flex-wrap gap-2">
            {selectedMarketEmails.map((email) => {
              const market = marketByEmail.get(email);
              return (
                <Badge
                  key={email}
                  variant="secondary"
                  className="max-w-full gap-1 rounded-md px-2 py-1 text-xs"
                >
                  <span className="min-w-0 truncate">
                    {market?.displayName ?? email}
                  </span>
                </Badge>
              );
            })}
          </div>
        ) : (
          <div className="break-all text-sm">
            {t("share.market.default", {
              defaultValue: "默认，不授权 Market",
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function ConnectInlineValue({
  label,
  value,
  displayValue,
  action,
  onCopy,
}: {
  label: string;
  value: string;
  displayValue?: string;
  action?: ReactNode;
  onCopy: () => void;
}) {
  return (
    <div className="min-w-0 rounded-md border border-border-default bg-background/60 px-3 py-2">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-2 flex items-center justify-between gap-2">
        <div className="min-w-0 flex-1">
          <code className="block min-w-0 break-all text-xs">
            {displayValue ?? (value || "-")}
          </code>
        </div>
        {action}
        <Button
          variant="ghost"
          size="icon"
          className="h-7 w-7 shrink-0"
          disabled={!value}
          onClick={onCopy}
        >
          <Copy className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  );
}

function SummaryLine({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-md border border-border-default/70 bg-muted/10 px-3 py-2">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 break-all text-sm">{value}</div>
    </div>
  );
}
