import { type ReactNode, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import type { PublicMarket, ShareRecord } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
import { EmailTagsInput } from "@/components/ui/tags-input";
import { cn } from "@/lib/utils";
import type { ShareProviderSalePricing } from "./ShareCard";
import {
  DEFAULT_PARALLEL_LIMIT,
  isPermanentExpiry,
  isUnlimitedParallelLimit,
  isUnlimitedTokenLimit,
  MIN_PARALLEL_LIMIT,
  PERMANENT_EXPIRES_AT,
  UNLIMITED_PARALLEL_LIMIT,
  UNLIMITED_TOKEN_LIMIT,
} from "@/utils/shareUtils";

interface EditShareDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  share: ShareRecord;
  markets: PublicMarket[];
  providerSalePricing: ShareProviderSalePricing[];
  marketsLoading: boolean;
  marketsError: string | null;
  readOnly?: boolean;
  subdomainReadOnly?: boolean;
  onRetryMarkets?: () => void;
  isBusy: boolean;
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

export function EditShareDialog({
  open,
  onOpenChange,
  share,
  markets,
  providerSalePricing,
  marketsLoading,
  marketsError,
  readOnly = false,
  subdomainReadOnly = false,
  onRetryMarkets,
  isBusy,
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
}: EditShareDialogProps) {
  const { t } = useTranslation();
  const [saving, setSaving] = useState(false);
  const [confirmFreeOpen, setConfirmFreeOpen] = useState(false);
  const [transferTargetEmail, setTransferTargetEmail] = useState<string | null>(
    null,
  );

  const [tokenLimitInput, setTokenLimitInput] = useState("");
  const [tokenLimitUnlimited, setTokenLimitUnlimited] = useState(false);
  const [lastFiniteTokenLimit, setLastFiniteTokenLimit] = useState(100000);
  const [parallelLimitInput, setParallelLimitInput] = useState("");
  const [parallelLimitUnlimited, setParallelLimitUnlimited] = useState(false);
  const [lastFiniteParallelLimit, setLastFiniteParallelLimit] = useState(
    DEFAULT_PARALLEL_LIMIT,
  );
  const [subdomainInput, setSubdomainInput] = useState("");
  const [descriptionInput, setDescriptionInput] = useState("");
  const [ownerEmailInput, setOwnerEmailInput] = useState("");
  const [shareToEmails, setShareToEmails] = useState<string[]>([]);
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
  const currentMarketAccessMode = share.marketAccessMode ?? "selected";
  const currentShareSalePricing = share.forSaleOfficialPricePercentByApp ?? {};
  const wasOpenRef = useRef(false);
  useEffect(() => {
    if (!open) {
      wasOpenRef.current = false;
      return;
    }
    if (wasOpenRef.current) return;
    wasOpenRef.current = true;
    setSaving(false);
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
    setDescriptionInput(share.description ?? "");
    setOwnerEmailInput(share.ownerEmail ?? "");
    setShareToEmails(currentNonMarketEmails);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, share, providerSalePricing]);

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
  const normalizedDescription = descriptionInput.trim();
  const descriptionDirty = normalizedDescription !== (share.description ?? "");
  const descriptionInvalid = normalizedDescription.length > 200;
  const normalizedOwnerEmail = ownerEmailInput.trim().toLowerCase();
  const ownerEmailDirty = normalizedOwnerEmail !== share.ownerEmail;
  const ownerEmailInvalid =
    !normalizedOwnerEmail ||
    !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(normalizedOwnerEmail);
  const normalizedShareTo = Array.from(
    new Set(
      shareToEmails
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
    ownerEmailDirty ||
    descriptionDirty ||
    expiryDirty ||
    subdomainDirty ||
    tokenLimitDirty ||
    parallelLimitDirty;
  const hasInvalidChanges =
    (aclDirty && shareToInvalid) ||
    salePricingInvalid ||
    (ownerEmailDirty && ownerEmailInvalid) ||
    (descriptionDirty && descriptionInvalid) ||
    (expiryDirty && expiryInvalid) ||
    (subdomainDirty && subdomainInvalid) ||
    (tokenLimitDirty && tokenLimitInvalid) ||
    (parallelLimitDirty && parallelLimitInvalid);

  const busy = isBusy || saving || readOnly;
  const marketDisabled = forSaleInput !== "Yes";
  const pricingDisabled = forSaleInput !== "Yes";

  const handleSave = async () => {
    if (!hasChanges || hasInvalidChanges || busy) return;
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
      if (ownerEmailDirty)
        await onUpdateOwnerEmail(share, normalizedOwnerEmail);
      if (descriptionDirty)
        await onUpdateDescription(share, normalizedDescription);
      if (expiryDirty) await onUpdateExpiration(share, expiryIso);
      if (subdomainDirty) await onUpdateSubdomain(share, subdomainInput.trim());
      if (tokenLimitDirty) await onUpdateTokenLimit(share, parsedTokenLimit);
      if (parallelLimitDirty)
        await onUpdateParallelLimit(share, parsedParallelLimit);
      onOpenChange(false);
    } finally {
      setSaving(false);
    }
  };

  const handleTransferOwner = async () => {
    if (!transferTargetEmail || busy) return;
    setSaving(true);
    try {
      await onTransferOwner(share, transferTargetEmail);
      setTransferTargetEmail(null);
      onOpenChange(false);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Dialog
        open={open}
        onOpenChange={(next) => {
          if (busy && !next) return;
          onOpenChange(next);
        }}
      >
        <DialogContent className="flex max-h-[90vh] w-full max-w-3xl flex-col gap-0 p-0">
          <DialogHeader className="flex flex-row items-start justify-between gap-4">
            <div className="flex flex-col gap-1">
              <DialogTitle>
                {t("share.editDialog.title", { defaultValue: "设置选项" })}
                <span className="ml-2 text-sm font-normal text-muted-foreground">
                  {t("share.settings", { defaultValue: "Settings" })}
                </span>
              </DialogTitle>
            </div>
            <label className="flex items-center gap-2 text-sm">
              <Checkbox
                id={`edit-share-auto-start-${share.id}`}
                checked={autoStartInput}
                disabled={busy}
                onCheckedChange={(checked) =>
                  setAutoStartInput(checked === true)
                }
              />
              <span className="cursor-pointer">
                {t("share.editDialog.autoStartTopLabel", {
                  defaultValue: "开机时自动启动此 Share",
                })}
              </span>
            </label>
          </DialogHeader>

          <div className="flex-1 space-y-4 overflow-y-auto px-6 py-5">
            <DialogSection
              title={t("share.ownerEmail", { defaultValue: "Owner Email" })}
              hint={t("share.ownerEmailCreateHint", {
                defaultValue:
                  "该邮箱会作为 share owner 上报到 router。router 页面使用相同邮箱登录后可查看 API Key 和编辑设置。",
              })}
              invalid={ownerEmailDirty && ownerEmailInvalid}
            >
              <Input
                type="email"
                value={ownerEmailInput}
                disabled={busy}
                onChange={(event) => setOwnerEmailInput(event.target.value)}
                placeholder="owner@example.com"
              />
            </DialogSection>

            <DialogSection
              title={t("share.subdomain", { defaultValue: "Subdomain" })}
              invalid={subdomainDirty && subdomainInvalid}
            >
              <Input
                value={subdomainInput}
                disabled={busy || subdomainReadOnly}
                onChange={(event) =>
                  setSubdomainInput(event.target.value.toLowerCase())
                }
              />
            </DialogSection>

            <DialogSection
              title={t("share.sharedWithEmails", { defaultValue: "Share To" })}
              hint={t("share.sharedWithEmailsHint", {
                defaultValue:
                  "配置多个邮箱后，这些邮箱登录 cc-switch-router dashboard 可以查看当前 share 的 API Key 明文。",
              })}
              invalid={shareToDirty && shareToInvalid}
            >
              <EmailTagsInput
                value={shareToEmails}
                disabled={busy}
                invalid={shareToDirty && shareToInvalid}
                onChange={setShareToEmails}
                placeholder={t("share.sharedWithEmailsPlaceholder", {
                  defaultValue: "friend@example.com, teammate@example.com",
                })}
                onPromote={(email) => setTransferTargetEmail(email)}
                promotableEmails={currentNonMarketEmails}
                promoteLabel={t("share.transferOwner.action", {
                  defaultValue: "设为 Owner",
                })}
              />
            </DialogSection>

            <DialogSection
              title={t("share.forSale", { defaultValue: "For Sale" })}
              hint={t("share.editDialog.forSaleDisableHint", {
                defaultValue:
                  "选择 Free 或 No 时，目标市场和模型定价将不可编辑。",
              })}
            >
              <div className="flex flex-wrap items-center gap-5">
                {(["Yes", "No", "Free"] as const).map((value) => {
                  const id = `edit-share-for-sale-${share.id}-${value}`;
                  return (
                    <label
                      key={value}
                      htmlFor={id}
                      className="flex cursor-pointer items-center gap-2 text-sm"
                    >
                      <input
                        id={id}
                        type="radio"
                        name={`edit-share-for-sale-${share.id}`}
                        value={value}
                        checked={forSaleInput === value}
                        disabled={busy}
                        onChange={() => {
                          if (value === "Free" && share.forSale !== "Free") {
                            setConfirmFreeOpen(true);
                            return;
                          }
                          setForSaleInput(value);
                        }}
                        className="h-4 w-4 accent-primary"
                      />
                      <span>
                        {t(`share.forSaleOptions.${value.toLowerCase()}`)}
                      </span>
                    </label>
                  );
                })}
              </div>
            </DialogSection>

            <DialogSection
              title={t("share.market.title", { defaultValue: "Market" })}
              hint={
                marketDisabled
                  ? t("share.market.forSaleRequired", {
                      defaultValue:
                        "Set ForSale to Yes before choosing a market.",
                    })
                  : t("share.market.description", {
                      defaultValue: "Choose one or more markets.",
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
                    disabled={busy || marketDisabled || marketsLoading}
                  >
                    <SelectTrigger
                      aria-label={t("share.market.select", {
                        defaultValue: "Select market",
                      })}
                    >
                      <SelectValue
                        placeholder={
                          marketsLoading
                            ? t("common.loading", { defaultValue: "Loading" })
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
                      busy ||
                      marketDisabled ||
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
                  disabled={busy || marketDisabled}
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
            </DialogSection>

            {providerSalePricing.length > 0 ? (
              <DialogSection
                title={t("share.modelPricingPercentTitle", {
                  defaultValue: "模型定价（Share 默认；供应商非空时优先）",
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
                        disabled={busy || pricingDisabled}
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
                            : t("share.forSaleOfficialPricePercentNoProvider", {
                                defaultValue: "无当前节点",
                              })
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
              </DialogSection>
            ) : null}

            <DialogSection
              title={t("share.description")}
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
                disabled={busy}
                onChange={(event) => setDescriptionInput(event.target.value)}
                placeholder={t("share.descriptionPlaceholder", {
                  defaultValue: "可选，信息将显示在 cc-switch-router 侧边栏",
                })}
              />
            </DialogSection>

            <div className="grid gap-4 md:grid-cols-3">
              <DialogSection
                title={t("share.expiresAt")}
                hint={t("share.expirationEditHint")}
                invalid={expiryDirty && expiryInvalid}
              >
                <div className="space-y-2">
                  <div className="flex items-center gap-2">
                    <input
                      id={`edit-share-expiry-permanent-${share.id}`}
                      type="radio"
                      name={`edit-share-expiry-mode-${share.id}`}
                      checked={expiryPermanent}
                      disabled={busy}
                      onChange={() => setExpiryPermanent(true)}
                      className="h-4 w-4 accent-primary"
                    />
                    <Label
                      htmlFor={`edit-share-expiry-permanent-${share.id}`}
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.expiry.permanent", {
                        defaultValue: "永久有效",
                      })}
                    </Label>
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      id={`edit-share-expiry-pick-${share.id}`}
                      type="radio"
                      name={`edit-share-expiry-mode-${share.id}`}
                      checked={!expiryPermanent}
                      disabled={busy}
                      onChange={() => setExpiryPermanent(false)}
                      className="h-4 w-4 accent-primary"
                    />
                    <Label
                      htmlFor={`edit-share-expiry-pick-${share.id}`}
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.expiry.pickDate", { defaultValue: "选择日期" })}
                    </Label>
                  </div>
                  <Input
                    type="date"
                    value={expiryPermanent ? "2099-12-31" : expiryDateInput}
                    disabled={busy || expiryPermanent}
                    onChange={(event) => setExpiryDateInput(event.target.value)}
                  />
                  <div className="grid grid-cols-2 gap-2">
                    <Input
                      type="number"
                      min={0}
                      max={23}
                      value={expiryPermanent ? "23" : expiryHourInput}
                      disabled={busy || expiryPermanent}
                      onChange={(event) =>
                        setExpiryHourInput(event.target.value)
                      }
                    />
                    <Input
                      type="number"
                      min={0}
                      max={59}
                      value={expiryPermanent ? "59" : expiryMinuteInput}
                      disabled={busy || expiryPermanent}
                      onChange={(event) =>
                        setExpiryMinuteInput(event.target.value)
                      }
                    />
                  </div>
                </div>
              </DialogSection>

              <DialogSection
                title={t("share.tokenLimit")}
                invalid={tokenLimitDirty && tokenLimitInvalid}
              >
                <div className="flex items-center gap-2">
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
                  <label className="flex items-center gap-2 whitespace-nowrap text-sm">
                    <Checkbox
                      id={`edit-share-token-limit-unlimited-${share.id}`}
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
                    <span className="cursor-pointer">
                      {t("share.unlimited", { defaultValue: "无上限" })}
                    </span>
                  </label>
                </div>
              </DialogSection>

              <DialogSection
                title={t("share.parallelLimit", {
                  defaultValue: "最大并发数",
                })}
                hint={t("share.parallelLimitHint")}
                invalid={parallelLimitDirty && parallelLimitInvalid}
              >
                <div className="flex items-center gap-2">
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
                  <label className="flex items-center gap-2 whitespace-nowrap text-sm">
                    <Checkbox
                      id={`edit-share-parallel-limit-unlimited-${share.id}`}
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
                    <span className="cursor-pointer">
                      {t("share.unlimited", { defaultValue: "无上限" })}
                    </span>
                  </label>
                </div>
              </DialogSection>
            </div>
          </div>

          <DialogFooter>
            <Button
              variant="outline"
              disabled={isBusy || saving}
              onClick={() => onOpenChange(false)}
            >
              {t("common.cancel", { defaultValue: "取消" })}
            </Button>
            {!readOnly ? (
              <Button
                disabled={!hasChanges || hasInvalidChanges || busy}
                onClick={() => void handleSave()}
              >
                {t("share.editDialog.save", { defaultValue: "保存设置" })}
              </Button>
            ) : null}
          </DialogFooter>
        </DialogContent>
      </Dialog>

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
      <ConfirmDialog
        isOpen={Boolean(transferTargetEmail)}
        title={t("share.transferOwner.confirmTitle", {
          defaultValue: "转移 Owner?",
        })}
        message={t("share.transferOwner.confirmMessage", {
          defaultValue:
            "将 {{target}} 升级为 owner，并把当前 owner {{owner}} 降级为 shareto。此操作会同步到 router。",
          target: transferTargetEmail ?? "",
          owner: share.ownerEmail,
        })}
        onConfirm={handleTransferOwner}
        onCancel={() => setTransferTargetEmail(null)}
      />
    </>
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

function DialogSection({
  title,
  hint,
  invalid = false,
  children,
}: {
  title: string;
  hint?: ReactNode;
  invalid?: boolean;
  children: ReactNode;
}) {
  return (
    <section
      className={cn(
        "rounded-lg border border-border-default bg-background/60 px-4 py-3",
        invalid && "border-destructive/60",
      )}
    >
      <div className="mb-2 text-sm font-semibold">{title}</div>
      {children}
      {hint ? (
        <div className="mt-2 flex items-center justify-between gap-2 text-xs text-muted-foreground">
          {hint}
        </div>
      ) : null}
    </section>
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
  const { t } = useTranslation();
  const marketByEmail = new Map(
    markets.map((market) => [market.email.toLowerCase(), market]),
  );

  if (marketAccessMode === "all") {
    return (
      <div className="text-sm text-muted-foreground">
        {t("share.market.allSelected", {
          defaultValue: "已选中所有 Market",
        })}
      </div>
    );
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
