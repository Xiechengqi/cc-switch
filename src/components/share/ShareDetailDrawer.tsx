import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff, RotateCcw, Save } from "lucide-react";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ShareDisplayStatusBadge } from "./ShareDisplayStatusBadge";
import {
  DEFAULT_PARALLEL_LIMIT,
  formatShareTokenUsage,
  formatUtcDateTime,
  getShareDisplayStatus,
  getShareTunnelRuntimeStatus,
  getShareUsageRatio,
  isUnlimitedParallelLimit,
  isPermanentExpiry,
  isUnlimitedTokenLimit,
  maskSensitive,
  MIN_PARALLEL_LIMIT,
  PERMANENT_EXPIRES_AT,
  resolveShareTunnelInfo,
  UNLIMITED_PARALLEL_LIMIT,
  UNLIMITED_TOKEN_LIMIT,
} from "@/utils/shareUtils";

interface ShareDetailDrawerProps {
  share: ShareRecord | null;
  tunnelStatus?: TunnelInfo | null;
  tunnelConfig: TunnelConfig;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onResetUsage: (share: ShareRecord) => void;
  onUpdateTokenLimit: (share: ShareRecord, tokenLimit: number) => void;
  onUpdateParallelLimit: (share: ShareRecord, parallelLimit: number) => void;
  onUpdateSubdomain: (share: ShareRecord, subdomain: string) => void;
  onUpdateApiKey: (share: ShareRecord, apiKey: string) => void;
  onUpdateDescription: (share: ShareRecord, description: string) => void;
  onUpdateForSale: (share: ShareRecord, forSale: "Yes" | "No" | "Free") => void;
  onUpdateExpiration: (share: ShareRecord, expiresAt: string) => void;
  onUpdateAcl: (share: ShareRecord, sharedWithEmails: string[]) => void;
  busy?: boolean;
}

export function ShareDetailDrawer({
  share,
  tunnelStatus,
  tunnelConfig,
  open,
  onOpenChange,
  onResetUsage,
  onUpdateTokenLimit,
  onUpdateParallelLimit,
  onUpdateSubdomain,
  onUpdateApiKey,
  onUpdateDescription,
  onUpdateForSale,
  onUpdateExpiration,
  onUpdateAcl,
  busy = false,
}: ShareDetailDrawerProps) {
  const { t } = useTranslation();
  const [revealToken, setRevealToken] = useState(false);
  const [tokenLimitInput, setTokenLimitInput] = useState("");
  const [subdomainInput, setSubdomainInput] = useState("");
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [descriptionInput, setDescriptionInput] = useState("");
  const [shareToInput, setShareToInput] = useState("");
  const [forSaleInput, setForSaleInput] = useState<"Yes" | "No" | "Free">("No");
  const [confirmFreeOpen, setConfirmFreeOpen] = useState(false);
  const [expiryDateInput, setExpiryDateInput] = useState("");
  const [expiryHourInput, setExpiryHourInput] = useState("");
  const [expiryMinuteInput, setExpiryMinuteInput] = useState("");
  const [expiryPermanent, setExpiryPermanent] = useState(false);
  const [tokenLimitUnlimited, setTokenLimitUnlimited] = useState(false);
  const [lastFiniteTokenLimit, setLastFiniteTokenLimit] = useState(100000);
  const [parallelLimitInput, setParallelLimitInput] = useState("");
  const [parallelLimitUnlimited, setParallelLimitUnlimited] = useState(false);
  const [lastFiniteParallelLimit, setLastFiniteParallelLimit] = useState(
    DEFAULT_PARALLEL_LIMIT,
  );

  useEffect(() => {
    setTokenLimitInput(share ? String(share.tokenLimit) : "");
    setTokenLimitUnlimited(isUnlimitedTokenLimit(share?.tokenLimit));
    setLastFiniteTokenLimit(
      share && !isUnlimitedTokenLimit(share.tokenLimit) && share.tokenLimit > 0
        ? share.tokenLimit
        : 100000,
    );
    setParallelLimitInput(share ? String(share.parallelLimit) : "");
    setParallelLimitUnlimited(isUnlimitedParallelLimit(share?.parallelLimit));
    setLastFiniteParallelLimit(
      share &&
        !isUnlimitedParallelLimit(share.parallelLimit) &&
        share.parallelLimit >= MIN_PARALLEL_LIMIT
        ? share.parallelLimit
        : DEFAULT_PARALLEL_LIMIT,
    );
    setSubdomainInput(share?.subdomain ?? "");
    setApiKeyInput(share?.shareToken ?? "");
    setDescriptionInput(share?.description ?? "");
    setShareToInput((share?.sharedWithEmails ?? []).join(", "));
    setForSaleInput(share?.forSale ?? "No");
    if (share?.expiresAt) {
      const permanent = isPermanentExpiry(share.expiresAt);
      setExpiryPermanent(permanent);
      const expires = new Date(share.expiresAt);
      if (!Number.isNaN(expires.getTime())) {
        const year = expires.getFullYear();
        const month = String(expires.getMonth() + 1).padStart(2, "0");
        const day = String(expires.getDate()).padStart(2, "0");
        setExpiryDateInput(`${year}-${month}-${day}`);
        setExpiryHourInput(String(expires.getHours()).padStart(2, "0"));
        setExpiryMinuteInput(String(expires.getMinutes()).padStart(2, "0"));
      }
    } else {
      setExpiryPermanent(false);
      setExpiryDateInput("");
      setExpiryHourInput("");
      setExpiryMinuteInput("");
    }
  }, [share]);

  if (!share) return null;

  const ratio = getShareUsageRatio(share);
  const tunnelDisplay = resolveShareTunnelInfo(share, tunnelConfig);
  const tunnelRuntimeStatus = getShareTunnelRuntimeStatus(share, tunnelStatus);
  const displayStatus = getShareDisplayStatus(
    share,
    Boolean(tunnelConfig.domain),
    tunnelStatus,
  );
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
        .filter(Boolean),
    ),
  ).sort();
  const shareToDirty =
    JSON.stringify(normalizedShareTo) !==
    JSON.stringify([...(share.sharedWithEmails ?? [])].sort());
  const shareToInvalid = normalizedShareTo.some(
    (email) => !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email),
  );
  const forSaleDirty = forSaleInput !== share.forSale;
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
  const expiryDirty = expiryIso && expiryIso !== share.expiresAt;
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

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="left-auto right-0 top-0 h-screen max-h-screen w-full max-w-2xl translate-x-0 translate-y-0 rounded-none border-l border-border-default p-0 data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right"
        overlayClassName="bg-black/30"
      >
        <DialogHeader className="space-y-3">
          <DialogTitle className="flex items-center gap-2">
            {share.name}
            <ShareDisplayStatusBadge status={displayStatus} />
          </DialogTitle>
          <DialogDescription>{t("share.editDescription")}</DialogDescription>
        </DialogHeader>
        <div className="flex-1 space-y-6 overflow-y-auto px-6 py-6">
          <section className="grid gap-4 md:grid-cols-2">
            <InfoField label={t("share.id")} value={share.id} />
            <InfoField
              label={t("share.apiKey")}
              value={
                share.forSale === "Free" || revealToken
                  ? share.shareToken
                  : maskSensitive(share.shareToken)
              }
              action={
                share.forSale !== "Free" ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => setRevealToken((prev) => !prev)}
                  >
                    {revealToken ? (
                      <EyeOff className="h-4 w-4" />
                    ) : (
                      <Eye className="h-4 w-4" />
                    )}
                  </Button>
                ) : null
              }
            />
            <InfoField
              label={t("share.requestsCount")}
              value={String(share.requestsCount)}
            />
            <InfoField
              label={t("share.status")}
              value={t(`share.displayStatuses.${displayStatus}`)}
            />
            <InfoField
              label={t("share.lastUsedAt")}
              value={
                share.lastUsedAt
                  ? formatUtcDateTime(share.lastUsedAt)
                  : t("share.never")
              }
            />
            <InfoField
              label={t("share.createdAt")}
              value={formatUtcDateTime(share.createdAt)}
            />
            <InfoField
              label={t("share.ownerEmail", { defaultValue: "Owner Email" })}
              value={share.ownerEmail || "-"}
            />
            <InfoField
              label={t("share.sharedWithEmails", {
                defaultValue: "Share To",
              })}
              value={
                share.sharedWithEmails.length
                  ? share.sharedWithEmails.join(", ")
                  : "-"
              }
            />
            <InfoField
              label={t("share.description")}
              value={share.description || "-"}
            />
            <InfoField
              label={t("share.forSale")}
              value={t(`share.forSaleOptions.${share.forSale.toLowerCase()}`)}
            />
            <InfoField
              label={t("share.expiresAt")}
              value={
                isPermanentExpiry(share.expiresAt)
                  ? t("share.expiry.permanentLabel")
                  : formatUtcDateTime(share.expiresAt)
              }
            />
            <InfoField
              label={t("share.tunnelUrl")}
              value={tunnelDisplay.tunnelUrl || "-"}
            />
            <InfoField
              label={t("share.subdomain")}
              value={tunnelDisplay.subdomain || "-"}
            />
            <InfoField
              label={t("share.remotePort")}
              value={
                tunnelStatus?.remotePort ? String(tunnelStatus.remotePort) : "-"
              }
            />
            <InfoField
              label={t("share.tunnelHealth")}
              value={t(`share.statuses.${tunnelRuntimeStatus}`)}
            />
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.accessControl", {
                defaultValue: "Access Control",
              })}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Label htmlFor="share-share-to">
                  {t("share.sharedWithEmails", {
                    defaultValue: "Share To",
                  })}
                </Label>
                <Textarea
                  id="share-share-to"
                  value={shareToInput}
                  onChange={(event) => setShareToInput(event.target.value)}
                  placeholder={t("share.sharedWithEmailsPlaceholder", {
                    defaultValue:
                      "friend@example.com, teammate@example.com",
                  })}
                />
                <div className="text-xs text-muted-foreground">
                  {t("share.sharedWithEmailsHint", {
                    defaultValue:
                      "配置多个邮箱后，这些邮箱登录 portr-rs dashboard 可以查看当前 share 的 API Key 明文。",
                  })}
                </div>
              </div>
              <Button
                type="button"
                className="self-start"
                disabled={busy || !shareToDirty || shareToInvalid}
                onClick={() => onUpdateAcl(share, normalizedShareTo)}
              >
                <Save className="mr-2 h-4 w-4" />
                {t("common.save", { defaultValue: "保存" })}
              </Button>
            </div>
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.forSaleSettings")}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Select
                  value={forSaleInput}
                  onValueChange={(value) => {
                    const next = value as "Yes" | "No" | "Free";
                    if (next === "Free" && share.forSale !== "Free") {
                      setConfirmFreeOpen(true);
                    } else {
                      setForSaleInput(next);
                    }
                  }}
                  disabled={busy}
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
                <div className="text-xs text-muted-foreground">
                  {t("share.forSaleHint")}
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || !forSaleDirty}
                  onClick={() => onUpdateForSale(share, forSaleInput)}
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveForSale")}
                </Button>
              </div>
            </div>
            <ConfirmDialog
              isOpen={confirmFreeOpen}
              title={t("share.forSaleFreeConfirmTitle")}
              message={t("share.forSaleFreeConfirmMessage")}
              variant="destructive"
              zIndex="top"
              onConfirm={() => {
                setForSaleInput("Free");
                setConfirmFreeOpen(false);
              }}
              onCancel={() => setConfirmFreeOpen(false)}
            />
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.descriptionSettings")}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Textarea
                  value={descriptionInput}
                  maxLength={200}
                  disabled={busy}
                  onChange={(event) => setDescriptionInput(event.target.value)}
                />
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>{t("share.descriptionHint")}</span>
                  <span>{normalizedDescription.length}/200</span>
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || descriptionInvalid || !descriptionDirty}
                  onClick={() =>
                    onUpdateDescription(share, normalizedDescription)
                  }
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveDescription")}
                </Button>
              </div>
            </div>
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.expirationSettings")}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_120px_120px_auto]">
              <div className="space-y-2">
                <div className="text-sm font-medium">
                  {t("share.expirationDate")}
                </div>
                <Input
                  type="date"
                  value={expiryPermanent ? "2099-12-31" : expiryDateInput}
                  disabled={busy || expiryPermanent}
                  onChange={(event) => setExpiryDateInput(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <div className="text-sm font-medium">
                  {t("share.expirationHour")}
                </div>
                <Input
                  type="number"
                  min={0}
                  max={23}
                  value={expiryPermanent ? "23" : expiryHourInput}
                  disabled={busy || expiryPermanent}
                  onChange={(event) => setExpiryHourInput(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <div className="text-sm font-medium">
                  {t("share.expirationMinute")}
                </div>
                <Input
                  type="number"
                  min={0}
                  max={59}
                  value={expiryPermanent ? "59" : expiryMinuteInput}
                  disabled={busy || expiryPermanent}
                  onChange={(event) => setExpiryMinuteInput(event.target.value)}
                />
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || !!expiryInvalid || !expiryDirty}
                  onClick={() => onUpdateExpiration(share, expiryIso)}
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveExpiration")}
                </Button>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox
                id="share-detail-expiry-permanent"
                checked={expiryPermanent}
                disabled={busy}
                onCheckedChange={(checked) =>
                  setExpiryPermanent(checked === true)
                }
              />
              <Label
                htmlFor="share-detail-expiry-permanent"
                className="cursor-pointer text-sm font-normal"
              >
                {t("share.expiry.permanent")}
              </Label>
            </div>
            <div className="text-xs text-muted-foreground">
              {t("share.expirationEditHint")}
            </div>
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.apiKeySettings")}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Input
                  value={apiKeyInput}
                  disabled={busy}
                  onChange={(event) => setApiKeyInput(event.target.value)}
                />
                <div className="text-xs text-muted-foreground">
                  {t("share.apiKeyHint")}
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || apiKeyInvalid || !apiKeyDirty}
                  onClick={() => onUpdateApiKey(share, apiKeyInput.trim())}
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveApiKey")}
                </Button>
              </div>
            </div>
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">
              {t("share.subdomainSettings")}
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Input
                  value={subdomainInput}
                  disabled={busy}
                  onChange={(event) =>
                    setSubdomainInput(event.target.value.toLowerCase())
                  }
                />
                <div className="text-xs text-muted-foreground">
                  {t("share.subdomainEditHint")}
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || subdomainInvalid || !subdomainDirty}
                  onClick={() =>
                    onUpdateSubdomain(share, subdomainInput.trim())
                  }
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveSubdomain")}
                </Button>
              </div>
            </div>
          </section>

          <section className="space-y-3">
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium">{t("share.quota")}</div>
              <div className="text-sm text-muted-foreground">
                {formatShareTokenUsage(share)}
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
            <div className="text-sm font-medium">{t("share.usageActions")}</div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <div className="text-sm font-medium">
                    {t("share.tokenLimit")}
                  </div>
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id="share-detail-token-limit-unlimited"
                      checked={tokenLimitUnlimited}
                      disabled={busy}
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
                      htmlFor="share-detail-token-limit-unlimited"
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.unlimited")}
                    </Label>
                  </div>
                </div>
                <Input
                  type="number"
                  min={1}
                  step={1}
                  value={tokenLimitInput}
                  disabled={busy || tokenLimitUnlimited}
                  onChange={(event) => {
                    setTokenLimitInput(event.target.value);
                    const next = Number.parseInt(event.target.value, 10);
                    if (Number.isFinite(next) && next > 0) {
                      setLastFiniteTokenLimit(next);
                    }
                  }}
                />
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || tokenLimitInvalid || !tokenLimitDirty}
                  onClick={() => onUpdateTokenLimit(share, parsedTokenLimit)}
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveTokenLimit")}
                </Button>
              </div>
            </div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <div className="text-sm font-medium">
                    {t("share.parallelLimit")}
                  </div>
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id="share-detail-parallel-limit-unlimited"
                      checked={parallelLimitUnlimited}
                      disabled={busy}
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
                      htmlFor="share-detail-parallel-limit-unlimited"
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.unlimited")}
                    </Label>
                  </div>
                </div>
                <Input
                  type="number"
                  min={MIN_PARALLEL_LIMIT}
                  step={1}
                  value={parallelLimitInput}
                  disabled={busy || parallelLimitUnlimited}
                  onChange={(event) => {
                    setParallelLimitInput(event.target.value);
                    const next = Number.parseInt(event.target.value, 10);
                    if (Number.isFinite(next) && next >= MIN_PARALLEL_LIMIT) {
                      setLastFiniteParallelLimit(next);
                    }
                  }}
                />
                <div className="text-xs text-muted-foreground">
                  {t("share.parallelLimitHint")}
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || parallelLimitInvalid || !parallelLimitDirty}
                  onClick={() =>
                    onUpdateParallelLimit(share, parsedParallelLimit)
                  }
                >
                  <Save className="h-4 w-4" />
                  {t("share.saveParallelLimit")}
                </Button>
              </div>
            </div>
            <div className="flex justify-start">
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => {
                  if (!window.confirm(t("share.confirmResetUsageMessage"))) {
                    return;
                  }
                  onResetUsage(share);
                }}
              >
                <RotateCcw className="h-4 w-4" />
                {t("share.resetUsage")}
              </Button>
            </div>
          </section>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function InfoField({
  label,
  value,
  action,
}: {
  label: string;
  value: string;
  action?: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-border-default bg-muted/20 px-4 py-3">
      <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
        {label}
      </div>
      <div className="mt-2 flex items-center justify-between gap-2 text-sm">
        <span className="break-all">{value}</span>
        {action}
      </div>
    </div>
  );
}
