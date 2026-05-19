import { type ReactNode, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Copy,
  Edit3,
  Eye,
  EyeOff,
  Play,
  Power,
  RotateCcw,
  Save,
  Trash2,
  X,
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
import { Checkbox } from "@/components/ui/checkbox";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { useProxyTakeoverStatus } from "@/lib/query/proxy";
import { copyText } from "@/lib/clipboard";
import { toast } from "sonner";
import { SHARE_REGIONS } from "@/config/shareRegions";
import { ShareDisplayStatusBadge } from "./ShareDisplayStatusBadge";
import { ShareRequestLogTable } from "./ShareRequestLogTable";
import {
  DEFAULT_PARALLEL_LIMIT,
  formatShareTokenUsage,
  formatUtcDateTime,
  getShareDisplayStatus,
  getShareTunnelRuntimeStatus,
  getShareUsageRatio,
  isUnlimitedParallelLimit,
  isPermanentExpiry,
  isShareActionAllowed,
  isUnlimitedTokenLimit,
  maskSensitive,
  MIN_PARALLEL_LIMIT,
  PERMANENT_EXPIRES_AT,
  resolveShareTunnelInfo,
  UNLIMITED_PARALLEL_LIMIT,
  UNLIMITED_TOKEN_LIMIT,
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
  ownerAuthenticated?: boolean;
  ownerLoginRequired?: boolean;
  onRetryMarkets?: () => void;
  onChangeOwner?: () => void;
  onVerifyOwner?: () => void;
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
  onUpdateApiKey: (share: ShareRecord, apiKey: string) => Promise<void> | void;
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
  ownerAuthenticated = false,
  ownerLoginRequired = false,
  onRetryMarkets,
  onChangeOwner,
  onVerifyOwner,
  onDelete,
  onEnable,
  onDisable,
  onResetUsage,
  onUpdateTokenLimit,
  onUpdateParallelLimit,
  onUpdateSubdomain,
  onUpdateApiKey,
  onUpdateDescription,
  onUpdateForSale,
  onUpdateShareSalePricing,
  onUpdateExpiration,
  onUpdateAutoStart,
  onUpdateAcl,
}: ShareCardProps) {
  const { t } = useTranslation();
  const [revealKey, setRevealKey] = useState(false);
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [confirmFreeOpen, setConfirmFreeOpen] = useState(false);
  const [tokenLimitInput, setTokenLimitInput] = useState("");
  const [tokenLimitUnlimited, setTokenLimitUnlimited] = useState(false);
  const [lastFiniteTokenLimit, setLastFiniteTokenLimit] = useState(100000);
  const [parallelLimitInput, setParallelLimitInput] = useState("");
  const [parallelLimitUnlimited, setParallelLimitUnlimited] = useState(false);
  const [lastFiniteParallelLimit, setLastFiniteParallelLimit] = useState(
    DEFAULT_PARALLEL_LIMIT,
  );
  const [subdomainInput, setSubdomainInput] = useState("");
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [descriptionInput, setDescriptionInput] = useState("");
  const [shareToInput, setShareToInput] = useState("");
  const [selectedMarketEmails, setSelectedMarketEmails] = useState<string[]>(
    [],
  );
  const [marketAccessModeInput, setMarketAccessModeInput] = useState<
    "selected" | "all"
  >("selected");
  const [marketSelectKey, setMarketSelectKey] = useState(0);
  const [forSaleInput, setForSaleInput] = useState<"Yes" | "No" | "Free">("No");
  const [salePricingInputs, setSalePricingInputs] = useState<
    Record<string, string>
  >({});
  const [autoStartInput, setAutoStartInput] = useState(false);
  const [expiryDateInput, setExpiryDateInput] = useState("");
  const [expiryHourInput, setExpiryHourInput] = useState("");
  const [expiryMinuteInput, setExpiryMinuteInput] = useState("");
  const [expiryPermanent, setExpiryPermanent] = useState(false);
  const ratio = getShareUsageRatio(share);
  const isBusy = pendingAction === share.id || saving;
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
  const currentMarketEmails = uniqueSorted(
    (share.sharedWithEmails ?? [])
      .map((email) => email.trim().toLowerCase())
      .filter((email) => marketEmailSet.has(email)),
  );
  const currentNonMarketEmails = uniqueSorted(
    (share.sharedWithEmails ?? [])
      .map((email) => email.trim().toLowerCase())
      .filter((email) => email && !marketEmailSet.has(email)),
  );

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
  const currentMarketAccessMode = share.marketAccessMode ?? "selected";
  const currentShareSalePricing = share.forSaleOfficialPricePercentByApp ?? {};
  const canDelete = share.status === "paused";
  const apiKeyDisplay =
    revealKey || isFree ? share.shareToken : maskSensitive(share.shareToken);
  useEffect(() => {
    setEditing(false);
    setSaving(false);
  }, [share.id]);

  useEffect(() => {
    if (editing) return;
    setTokenLimitInput(String(share.tokenLimit));
    setTokenLimitUnlimited(isUnlimitedTokenLimit(share.tokenLimit));
    setLastFiniteTokenLimit(
      !isUnlimitedTokenLimit(share.tokenLimit) && share.tokenLimit > 0
        ? share.tokenLimit
        : 100000,
    );
    setParallelLimitInput(String(share.parallelLimit));
    setParallelLimitUnlimited(isUnlimitedParallelLimit(share.parallelLimit));
    setLastFiniteParallelLimit(
      !isUnlimitedParallelLimit(share.parallelLimit) &&
        share.parallelLimit >= MIN_PARALLEL_LIMIT
        ? share.parallelLimit
        : DEFAULT_PARALLEL_LIMIT,
    );
    setSubdomainInput(share.subdomain ?? "");
    setApiKeyInput(share.shareToken);
    setDescriptionInput(share.description ?? "");
    setShareToInput(currentNonMarketEmails.join(", "));
    setSelectedMarketEmails(currentMarketEmails);
    setMarketAccessModeInput(currentMarketAccessMode);
    setForSaleInput(share.forSale);
    setSalePricingInputs(
      salePricingInputValues(providerSalePricing, currentShareSalePricing),
    );
    setAutoStartInput(share.autoStart);
    const permanent = isPermanentExpiry(share.expiresAt);
    setExpiryPermanent(permanent);
    const expires = new Date(share.expiresAt);
    if (!Number.isNaN(expires.getTime())) {
      setExpiryDateInput(
        `${expires.getFullYear()}-${String(expires.getMonth() + 1).padStart(
          2,
          "0",
        )}-${String(expires.getDate()).padStart(2, "0")}`,
      );
      setExpiryHourInput(String(expires.getHours()).padStart(2, "0"));
      setExpiryMinuteInput(String(expires.getMinutes()).padStart(2, "0"));
    } else {
      setExpiryDateInput("");
      setExpiryHourInput("");
      setExpiryMinuteInput("");
    }
  }, [editing, share, providerSalePricing]);

  useEffect(() => {
    if (editing) return;
    setShareToInput(currentNonMarketEmails.join(", "));
    setSelectedMarketEmails(currentMarketEmails);
    setMarketAccessModeInput(currentMarketAccessMode);
  }, [editing, share.sharedWithEmails, markets, currentMarketAccessMode]);

  const parsedTokenLimit = Number.parseInt(tokenLimitInput, 10);
  const tokenLimitDirty =
    Number.isFinite(parsedTokenLimit) && parsedTokenLimit !== share.tokenLimit;
  const tokenLimitInvalid =
    tokenLimitInput.trim().length === 0 ||
    !Number.isFinite(parsedTokenLimit) ||
    (parsedTokenLimit <= 0 && parsedTokenLimit !== UNLIMITED_TOKEN_LIMIT);
  const parsedParallelLimit = Number.parseInt(parallelLimitInput, 10);
  const parallelLimitDirty =
    Number.isFinite(parsedParallelLimit) &&
    parsedParallelLimit !== share.parallelLimit;
  const parallelLimitInvalid =
    parallelLimitInput.trim().length === 0 ||
    !Number.isFinite(parsedParallelLimit) ||
    (parsedParallelLimit !== UNLIMITED_PARALLEL_LIMIT &&
      parsedParallelLimit < MIN_PARALLEL_LIMIT);
  const subdomainDirty = subdomainInput.trim() !== (share.subdomain ?? "");
  const subdomainInvalid =
    subdomainInput.trim().length < 3 ||
    !/^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])?$/.test(subdomainInput.trim()) ||
    ["admin", "api", "www", "cdn-cgi"].includes(subdomainInput.trim());
  const apiKeyDirty = apiKeyInput.trim() !== share.shareToken;
  const apiKeyInvalid =
    apiKeyInput.trim().length < 8 ||
    !/^[A-Za-z0-9._-]{8,128}$/.test(apiKeyInput.trim());
  const normalizedDescription = descriptionInput.trim();
  const descriptionDirty = normalizedDescription !== (share.description ?? "");
  const descriptionInvalid = normalizedDescription.length > 200;
  const normalizedShareTo = Array.from(
    new Set(
      shareToInput
        .split(/[\n,]/)
        .map((value) => value.trim().toLowerCase())
        .filter((value) => value && !marketEmailSet.has(value)),
    ),
  ).sort();
  const shareToDirty =
    JSON.stringify(normalizedShareTo) !==
    JSON.stringify(currentNonMarketEmails);
  const normalizedSelectedMarketEmails = uniqueSorted(
    marketAccessModeInput === "all"
      ? []
      : selectedMarketEmails.filter((email) => marketEmailSet.has(email)),
  );
  const marketDirty =
    marketAccessModeInput !== currentMarketAccessMode ||
    (marketAccessModeInput === "selected" &&
      JSON.stringify(normalizedSelectedMarketEmails) !==
        JSON.stringify(currentMarketEmails));
  const nextAclEmails = uniqueSorted([
    ...normalizedShareTo,
    ...(marketAccessModeInput === "all" ? [] : normalizedSelectedMarketEmails),
  ]);
  const aclDirty = shareToDirty || marketDirty;
  const shareToInvalid = normalizedShareTo.some(
    (email) => !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email),
  );
  const forSaleDirty = forSaleInput !== share.forSale;
  const salePricingCurrentValues = salePricingInputValues(
    providerSalePricing,
    currentShareSalePricing,
  );
  const salePricingDirty = providerSalePricing.some(
    (item) =>
      (salePricingInputs[item.app] ?? "") !==
      (salePricingCurrentValues[item.app] ?? ""),
  );
  const salePricingInvalid = providerSalePricing.some((item) => {
    const value = (salePricingInputs[item.app] ?? "").trim();
    if (value === "") return false;
    if (!/^\d+$/.test(value)) return true;
    const parsed = Number.parseInt(value, 10);
    return parsed < 1 || parsed > 100;
  });
  const autoStartDirty = autoStartInput !== share.autoStart;
  const parsedExpiryHour = Number.parseInt(expiryHourInput, 10);
  const parsedExpiryMinute = Number.parseInt(expiryMinuteInput, 10);
  const computedExpiryIso =
    expiryDateInput &&
    Number.isFinite(parsedExpiryHour) &&
    Number.isFinite(parsedExpiryMinute) &&
    parsedExpiryHour >= 0 &&
    parsedExpiryHour <= 23 &&
    parsedExpiryMinute >= 0 &&
    parsedExpiryMinute <= 59
      ? new Date(
          Number.parseInt(expiryDateInput.slice(0, 4), 10),
          Number.parseInt(expiryDateInput.slice(5, 7), 10) - 1,
          Number.parseInt(expiryDateInput.slice(8, 10), 10),
          parsedExpiryHour,
          parsedExpiryMinute,
          0,
          0,
        ).toISOString()
      : "";
  const expiryIso = expiryPermanent ? PERMANENT_EXPIRES_AT : computedExpiryIso;
  const currentExpiryMs = new Date(share.expiresAt).getTime();
  const nextExpiryMs = expiryIso ? new Date(expiryIso).getTime() : NaN;
  const expiryDirty = expiryPermanent
    ? !isPermanentExpiry(share.expiresAt)
    : Boolean(
        expiryIso &&
          Number.isFinite(currentExpiryMs) &&
          Number.isFinite(nextExpiryMs) &&
          currentExpiryMs !== nextExpiryMs,
      );
  const expiryInvalid = expiryPermanent
    ? false
    : !expiryDateInput ||
      !Number.isFinite(parsedExpiryHour) ||
      !Number.isFinite(parsedExpiryMinute) ||
      parsedExpiryHour < 0 ||
      parsedExpiryHour > 23 ||
      parsedExpiryMinute < 0 ||
      parsedExpiryMinute > 59 ||
      Number.isNaN(new Date(expiryIso).getTime()) ||
      new Date(expiryIso).getTime() <= Date.now();
  const hasChanges =
    aclDirty ||
    forSaleDirty ||
    salePricingDirty ||
    autoStartDirty ||
    descriptionDirty ||
    expiryDirty ||
    apiKeyDirty ||
    subdomainDirty ||
    tokenLimitDirty ||
    parallelLimitDirty;
  const hasInvalidChanges =
    (aclDirty && shareToInvalid) ||
    salePricingInvalid ||
    (descriptionDirty && descriptionInvalid) ||
    (expiryDirty && expiryInvalid) ||
    (apiKeyDirty && apiKeyInvalid) ||
    (subdomainDirty && subdomainInvalid) ||
    (tokenLimitDirty && tokenLimitInvalid) ||
    (parallelLimitDirty && parallelLimitInvalid);

  const handleCopy = async (value: string, key: string) => {
    await copyText(value);
    toast.success(t(key));
  };

  const handleSave = async () => {
    if (!editing || !hasChanges || hasInvalidChanges || isBusy) return;
    setSaving(true);
    try {
      if (aclDirty)
        await onUpdateAcl(share, nextAclEmails, marketAccessModeInput);
      if (forSaleDirty) await onUpdateForSale(share, forSaleInput);
      if (salePricingDirty) {
        await onUpdateShareSalePricing(
          share,
          parseSalePricingInputs(salePricingInputs),
        );
      }
      if (autoStartDirty) await onUpdateAutoStart(share, autoStartInput);
      if (descriptionDirty)
        await onUpdateDescription(share, normalizedDescription);
      if (expiryDirty) await onUpdateExpiration(share, expiryIso);
      if (apiKeyDirty) await onUpdateApiKey(share, apiKeyInput.trim());
      if (subdomainDirty) await onUpdateSubdomain(share, subdomainInput.trim());
      if (tokenLimitDirty) await onUpdateTokenLimit(share, parsedTokenLimit);
      if (parallelLimitDirty)
        await onUpdateParallelLimit(share, parsedParallelLimit);
      setEditing(false);
    } finally {
      setSaving(false);
    }
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
            {share.ownerEmail && ownerLoginRequired ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={isBusy}
                onClick={onVerifyOwner}
              >
                {t("share.ownerLogin.reverifyAction", {
                  defaultValue: "重新验证 Owner 邮箱",
                })}
              </Button>
            ) : share.ownerEmail && ownerAuthenticated ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={isBusy}
                onClick={onChangeOwner}
              >
                {t("share.ownerChange.action", {
                  defaultValue: "Change Owner Email",
                })}
              </Button>
            ) : null}
            {share.ownerEmail && !ownerAuthenticated ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={isBusy}
                onClick={onVerifyOwner}
              >
                {t("share.ownerLogin.reverifyAction", {
                  defaultValue: "重新验证 Owner 邮箱",
                })}
              </Button>
            ) : null}
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
              onClick={() => setEditing((prev) => !prev)}
            >
              <Edit3 className="h-4 w-4" />
              {editing
                ? t("share.exitEdit", { defaultValue: "退出编辑" })
                : t("share.edit")}
            </Button>
            {editing ? (
              <Button
                variant="outline"
                size="sm"
                disabled={isBusy || !hasChanges || hasInvalidChanges}
                onClick={() => void handleSave()}
              >
                <Save className="h-4 w-4" />
                {t("common.save", { defaultValue: "保存" })}
              </Button>
            ) : null}
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
              displayValue={editing ? undefined : tunnelDisplay.subdomain}
              editor={
                editing ? (
                  <Input
                    value={subdomainInput}
                    disabled={isBusy}
                    onChange={(event) =>
                      setSubdomainInput(event.target.value.toLowerCase())
                    }
                  />
                ) : null
              }
              invalid={subdomainDirty && subdomainInvalid}
              onCopy={() =>
                void handleCopy(
                  tunnelDisplay.subdomain,
                  "share.toast.copySubdomain",
                )
              }
            />
            <ConnectInlineValue
              label={t("share.apiKey")}
              value={share.shareToken}
              displayValue={apiKeyDisplay}
              editor={
                editing ? (
                  <Input
                    value={apiKeyInput}
                    disabled={isBusy}
                    onChange={(event) => setApiKeyInput(event.target.value)}
                  />
                ) : null
              }
              invalid={apiKeyDirty && apiKeyInvalid}
              action={
                !isFree ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 shrink-0"
                    onClick={() => setRevealKey((prev) => !prev)}
                  >
                    {revealKey ? (
                      <EyeOff className="h-3.5 w-3.5" />
                    ) : (
                      <Eye className="h-3.5 w-3.5" />
                    )}
                  </Button>
                ) : null
              }
              onCopy={() =>
                void handleCopy(share.shareToken, "share.toast.copyApiKey")
              }
            />
          </div>
        </section>

        <section className="space-y-4 border-t border-border-default/70 pt-4">
          <div className="flex items-center justify-between gap-3">
            <div className="text-sm font-semibold">
              {t("share.settings", { defaultValue: "设置项" })}
            </div>
          </div>

          {!isUnlimitedTokenLimit(share.tokenLimit) ? (
            <div className="h-2 rounded-full bg-muted">
              <div
                className="h-2 rounded-full bg-blue-500"
                style={{ width: `${Math.max(4, ratio * 100)}%` }}
              />
            </div>
          ) : null}

          {!editing ? (
            <div className="grid gap-2 md:grid-cols-3">
              <SummaryLine
                label={t("share.ownerEmail", { defaultValue: "Owner Email" })}
                value={share.ownerEmail || "-"}
              />
              <SummaryLine
                label={t("share.sharedWithEmails", {
                  defaultValue: "Share To",
                })}
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
                selectedMarketEmails={normalizedSelectedMarketEmails}
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
          ) : (
            <div className="grid gap-4 lg:grid-cols-2">
              <EditableField
                label={t("share.autoStart")}
                hint={t("share.autoStartHint")}
              >
                <div className="flex min-h-10 items-center gap-2 rounded-md border border-border-default px-3 py-2">
                  <Checkbox
                    id={`share-auto-start-${share.id}`}
                    checked={autoStartInput}
                    disabled={isBusy}
                    onCheckedChange={(checked) =>
                      setAutoStartInput(checked === true)
                    }
                  />
                  <Label
                    htmlFor={`share-auto-start-${share.id}`}
                    className="cursor-pointer text-sm font-normal"
                  >
                    {t("share.autoStart")}
                  </Label>
                </div>
              </EditableField>

              <EditableField
                label={t("share.sharedWithEmails", {
                  defaultValue: "Share To",
                })}
                hint={t("share.sharedWithEmailsHint", {
                  defaultValue:
                    "配置多个邮箱后，这些邮箱登录 cc-switch-router dashboard 可以查看当前 share 的 API Key 明文。",
                })}
                invalid={shareToDirty && shareToInvalid}
              >
                <Textarea
                  value={shareToInput}
                  disabled={isBusy}
                  onChange={(event) => setShareToInput(event.target.value)}
                  placeholder={t("share.sharedWithEmailsPlaceholder", {
                    defaultValue: "friend@example.com, teammate@example.com",
                  })}
                />
              </EditableField>

              <EditableField
                label={t("share.forSale")}
                hint={t("share.forSaleHint")}
              >
                <Select
                  value={forSaleInput}
                  onValueChange={(value) => {
                    const next = value as "Yes" | "No" | "Free";
                    if (next === "Free" && share.forSale !== "Free") {
                      setConfirmFreeOpen(true);
                      return;
                    }
                    setForSaleInput(next);
                  }}
                  disabled={isBusy}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="No">
                      {t("share.forSaleOptions.no")}
                    </SelectItem>
                    <SelectItem value="Yes">
                      {t("share.forSaleOptions.yes")}
                    </SelectItem>
                    <SelectItem value="Free">
                      {t("share.forSaleOptions.free")}
                    </SelectItem>
                  </SelectContent>
                </Select>
              </EditableField>

              {providerSalePricing.length > 0 ? (
                <EditableField
                  label={t("share.modelPricingPercentTitle", {
                    defaultValue:
                      "模型定价（Share 默认；供应商非空时优先）",
                  })}
                  invalid={salePricingInvalid}
                >
                  <div className="grid gap-3 md:grid-cols-3">
                    {providerSalePricing.map((item) => (
                      <div key={item.app} className="space-y-1">
                        <Label className="text-xs text-muted-foreground">
                          {item.label}
                        </Label>
                        <Input
                          type="number"
                          min="1"
                          max="100"
                          step="1"
                          inputMode="numeric"
                          value={salePricingInputs[item.app] ?? ""}
                          disabled={isBusy}
                          onChange={(event) =>
                            setSalePricingInputs((current) => ({
                              ...current,
                              [item.app]: event.target.value,
                            }))
                          }
                          placeholder={
                            item.providerName
                              ? t("share.forSaleOfficialPricePercentEmpty", {
                                  defaultValue: "未设置",
                                })
                              : t(
                                  "share.forSaleOfficialPricePercentNoProvider",
                                  {
                                    defaultValue: "无当前节点",
                                  },
                                )
                          }
                        />
                        <div className="truncate text-xs text-muted-foreground">
                          {item.percent == null
                            ? (item.providerName ?? "-")
                            : t("share.providerPricingOverrideHint", {
                                defaultValue:
                                  "{{provider}} provider override: {{percent}}%",
                                provider: item.providerName ?? item.label,
                                percent: item.percent,
                              })}
                        </div>
                      </div>
                    ))}
                  </div>
                </EditableField>
              ) : null}

              <EditableField
                label={t("share.market.title", { defaultValue: "Market" })}
                hint={
                  forSaleInput === "Yes"
                    ? t("share.market.description", {
                        defaultValue: "Choose one or more markets.",
                      })
                    : t("share.market.forSaleRequired", {
                        defaultValue:
                          "Set ForSale to Yes before choosing a market.",
                      })
                }
              >
                <div className="space-y-3">
                  <div className="flex gap-2">
                    <Select
                      key={marketSelectKey}
                      onValueChange={(value) => {
                        if (value === "__all__") {
                          setMarketAccessModeInput("all");
                          setSelectedMarketEmails([]);
                          setMarketSelectKey((current) => current + 1);
                          return;
                        }
                        setMarketAccessModeInput("selected");
                        setSelectedMarketEmails((current) =>
                          uniqueSorted([...current, value.toLowerCase()]),
                        );
                        setMarketSelectKey((current) => current + 1);
                      }}
                      disabled={
                        isBusy || forSaleInput !== "Yes" || marketsLoading
                      }
                    >
                      <SelectTrigger
                        aria-label={t("share.market.select", {
                          defaultValue: "Select market",
                        })}
                      >
                        <SelectValue
                          placeholder={
                            marketsLoading
                              ? t("common.loading", {
                                  defaultValue: "Loading",
                                })
                              : t("share.market.select", {
                                  defaultValue: "Select market",
                                })
                          }
                        />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="__all__">
                          {t("share.market.all", { defaultValue: "All" })}
                        </SelectItem>
                        {markets.map((market) => (
                          <SelectItem key={market.id} value={market.email}>
                            {market.displayName}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <Button
                      type="button"
                      variant="outline"
                      disabled={
                        isBusy ||
                        (marketAccessModeInput === "selected" &&
                          selectedMarketEmails.length === 0)
                      }
                      onClick={() => {
                        setMarketAccessModeInput("selected");
                        setSelectedMarketEmails([]);
                      }}
                    >
                      {t("share.market.restore", { defaultValue: "还原" })}
                    </Button>
                  </div>
                  <MarketTags
                    markets={markets}
                    marketAccessMode={marketAccessModeInput}
                    selectedMarketEmails={normalizedSelectedMarketEmails}
                    removable
                    disabled={isBusy}
                    onRemove={(email) =>
                      setSelectedMarketEmails((current) =>
                        current.filter((item) => item !== email),
                      )
                    }
                  />
                  {marketAccessModeInput !== "all" &&
                  normalizedSelectedMarketEmails.length === 0 ? (
                    <div className="text-sm text-muted-foreground">
                      {t("share.market.default", {
                        defaultValue: "默认，不授权 Market",
                      })}
                    </div>
                  ) : null}
                  {marketsError ? (
                    <button
                      type="button"
                      className="text-xs text-destructive underline"
                      onClick={onRetryMarkets}
                    >
                      {marketsError}
                    </button>
                  ) : null}
                </div>
              </EditableField>

              <EditableField
                label={t("share.description")}
                hint={
                  <>
                    <span>{t("share.descriptionHint")}</span>
                    <span>{normalizedDescription.length}/200</span>
                  </>
                }
                invalid={descriptionDirty && descriptionInvalid}
              >
                <Textarea
                  value={descriptionInput}
                  maxLength={200}
                  disabled={isBusy}
                  onChange={(event) => setDescriptionInput(event.target.value)}
                />
              </EditableField>

              <EditableField
                label={t("share.expiresAt")}
                hint={t("share.expirationEditHint")}
                invalid={expiryDirty && expiryInvalid}
              >
                <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_90px_90px]">
                  <Input
                    type="date"
                    value={expiryPermanent ? "2099-12-31" : expiryDateInput}
                    disabled={isBusy || expiryPermanent}
                    onChange={(event) => setExpiryDateInput(event.target.value)}
                  />
                  <Input
                    type="number"
                    min={0}
                    max={23}
                    value={expiryPermanent ? "23" : expiryHourInput}
                    disabled={isBusy || expiryPermanent}
                    onChange={(event) => setExpiryHourInput(event.target.value)}
                  />
                  <Input
                    type="number"
                    min={0}
                    max={59}
                    value={expiryPermanent ? "59" : expiryMinuteInput}
                    disabled={isBusy || expiryPermanent}
                    onChange={(event) =>
                      setExpiryMinuteInput(event.target.value)
                    }
                  />
                </div>
                <div className="mt-2 flex items-center gap-2">
                  <Checkbox
                    id={`share-expiry-permanent-${share.id}`}
                    checked={expiryPermanent}
                    disabled={isBusy}
                    onCheckedChange={(checked) =>
                      setExpiryPermanent(checked === true)
                    }
                  />
                  <Label
                    htmlFor={`share-expiry-permanent-${share.id}`}
                    className="cursor-pointer text-sm font-normal"
                  >
                    {t("share.expiry.permanent")}
                  </Label>
                </div>
              </EditableField>

              <EditableField
                label={t("share.tokenLimit")}
                invalid={tokenLimitDirty && tokenLimitInvalid}
              >
                <div className="flex items-center justify-between gap-3">
                  <Input
                    type="number"
                    min={1}
                    step={1}
                    value={tokenLimitInput}
                    disabled={isBusy || tokenLimitUnlimited}
                    onChange={(event) => {
                      setTokenLimitInput(event.target.value);
                      const next = Number.parseInt(event.target.value, 10);
                      if (Number.isFinite(next) && next > 0) {
                        setLastFiniteTokenLimit(next);
                      }
                    }}
                  />
                  <div className="flex items-center gap-2 whitespace-nowrap">
                    <Checkbox
                      id={`share-token-limit-unlimited-${share.id}`}
                      checked={tokenLimitUnlimited}
                      disabled={isBusy}
                      onCheckedChange={(checked) => {
                        const next = checked === true;
                        setTokenLimitUnlimited(next);
                        if (next) {
                          if (
                            Number.isFinite(parsedTokenLimit) &&
                            parsedTokenLimit > 0
                          ) {
                            setLastFiniteTokenLimit(parsedTokenLimit);
                          }
                          setTokenLimitInput(String(UNLIMITED_TOKEN_LIMIT));
                          return;
                        }
                        setTokenLimitInput(String(lastFiniteTokenLimit));
                      }}
                    />
                    <Label
                      htmlFor={`share-token-limit-unlimited-${share.id}`}
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.unlimited")}
                    </Label>
                  </div>
                </div>
              </EditableField>

              <EditableField
                label={t("share.parallelLimit")}
                hint={t("share.parallelLimitHint")}
                invalid={parallelLimitDirty && parallelLimitInvalid}
              >
                <div className="flex items-center justify-between gap-3">
                  <Input
                    type="number"
                    min={MIN_PARALLEL_LIMIT}
                    step={1}
                    value={parallelLimitInput}
                    disabled={isBusy || parallelLimitUnlimited}
                    onChange={(event) => {
                      setParallelLimitInput(event.target.value);
                      const next = Number.parseInt(event.target.value, 10);
                      if (Number.isFinite(next) && next >= MIN_PARALLEL_LIMIT) {
                        setLastFiniteParallelLimit(next);
                      }
                    }}
                  />
                  <div className="flex items-center gap-2 whitespace-nowrap">
                    <Checkbox
                      id={`share-parallel-limit-unlimited-${share.id}`}
                      checked={parallelLimitUnlimited}
                      disabled={isBusy}
                      onCheckedChange={(checked) => {
                        const next = checked === true;
                        setParallelLimitUnlimited(next);
                        if (next) {
                          if (
                            Number.isFinite(parsedParallelLimit) &&
                            parsedParallelLimit >= MIN_PARALLEL_LIMIT
                          ) {
                            setLastFiniteParallelLimit(parsedParallelLimit);
                          }
                          setParallelLimitInput(
                            String(UNLIMITED_PARALLEL_LIMIT),
                          );
                          return;
                        }
                        setParallelLimitInput(String(lastFiniteParallelLimit));
                      }}
                    />
                    <Label
                      htmlFor={`share-parallel-limit-unlimited-${share.id}`}
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.unlimited")}
                    </Label>
                  </div>
                </div>
              </EditableField>
            </div>
          )}
        </section>

        <ShareRequestLogTable shareId={share.id} />

        <ConfirmDialog
          isOpen={confirmFreeOpen}
          title={t("share.forSaleFreeConfirmTitle")}
          message={t("share.forSaleFreeConfirmMessage")}
          variant="destructive"
          onConfirm={() => {
            setForSaleInput("Free");
            setConfirmFreeOpen(false);
          }}
          onCancel={() => setConfirmFreeOpen(false)}
        />
      </CardContent>
    </Card>
  );
}

function uniqueSorted(values: string[]) {
  return Array.from(
    new Set(values.map((value) => value.trim().toLowerCase()).filter(Boolean)),
  ).sort();
}

function salePricingInputValues(
  providerSalePricing: ShareProviderSalePricing[],
  sharePricing: Record<string, number>,
) {
  return Object.fromEntries(
    providerSalePricing.map((item) => {
      const percent = sharePricing[item.app];
      return [item.app, percent == null ? "" : String(percent)];
    }),
  );
}

function parseSalePricingInputs(inputs: Record<string, string>) {
  return Object.fromEntries(
    Object.entries(inputs)
      .map(([app, value]) => [app, value.trim()] as const)
      .filter(([, value]) => value !== "")
      .map(([app, value]) => [app, Number.parseInt(value, 10)]),
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

  return (
    <div className="min-w-0 rounded-md border border-border-default/70 bg-muted/10 px-3 py-2">
      <div className="text-xs text-muted-foreground">
        {t("share.market.title", { defaultValue: "Market" })}
      </div>
      <div className="mt-2">
        {marketAccessMode === "all" ? (
          <MarketAllNotice />
        ) : selectedMarketEmails.length ? (
          <MarketTags
            markets={markets}
            selectedMarketEmails={selectedMarketEmails}
          />
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

function MarketTags({
  markets,
  marketAccessMode = "selected",
  selectedMarketEmails,
  removable = false,
  disabled = false,
  onRemove,
}: {
  markets: PublicMarket[];
  marketAccessMode?: "selected" | "all";
  selectedMarketEmails: string[];
  removable?: boolean;
  disabled?: boolean;
  onRemove?: (email: string) => void;
}) {
  const marketByEmail = new Map(
    markets.map((market) => [market.email.toLowerCase(), market]),
  );

  if (marketAccessMode === "all") {
    return <MarketAllNotice />;
  }

  if (selectedMarketEmails.length === 0) return null;

  return (
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
            {removable ? (
              <button
                type="button"
                className="rounded-sm p-0.5 hover:bg-background/70 disabled:opacity-50"
                disabled={disabled}
                onClick={() => onRemove?.(email)}
                aria-label={`Remove ${market?.displayName ?? email}`}
              >
                <X className="h-3 w-3" />
              </button>
            ) : null}
          </Badge>
        );
      })}
    </div>
  );
}

function MarketAllNotice() {
  const { t } = useTranslation();
  return (
    <div className="text-sm text-muted-foreground">
      {t("share.market.allSelected", {
        defaultValue: "已选中所有 Market",
      })}
    </div>
  );
}

function ConnectInlineValue({
  label,
  value,
  displayValue,
  editor,
  action,
  invalid = false,
  onCopy,
}: {
  label: string;
  value: string;
  displayValue?: string;
  editor?: ReactNode;
  action?: ReactNode;
  invalid?: boolean;
  onCopy: () => void;
}) {
  return (
    <div
      className={cn(
        "min-w-0 rounded-md border border-border-default bg-background/60 px-3 py-2",
        invalid && "border-destructive/60",
      )}
    >
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-2 flex items-center justify-between gap-2">
        <div className="min-w-0 flex-1">
          {editor ?? (
            <code className="block min-w-0 break-all text-xs">
              {displayValue ?? (value || "-")}
            </code>
          )}
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

function EditableField({
  label,
  hint,
  invalid = false,
  children,
}: {
  label: string;
  hint?: ReactNode;
  invalid?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      className={cn(
        "min-w-0 rounded-lg border border-border-default bg-background/60 px-3 py-3",
        invalid && "border-destructive/60",
      )}
    >
      <div className="mb-2 text-xs text-muted-foreground">{label}</div>
      {children}
      {hint ? (
        <div className="mt-2 flex items-center justify-between gap-2 text-xs text-muted-foreground">
          {hint}
        </div>
      ) : null}
    </div>
  );
}
