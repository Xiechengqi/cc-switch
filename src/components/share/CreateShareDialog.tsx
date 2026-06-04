import { useEffect, useMemo, useRef, useState } from "react";
import { zodResolver } from "@hookform/resolvers/zod";
import { useForm } from "react-hook-form";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronRight, X } from "lucide-react";
import type {
  AppId,
  CreateShareParams,
  PublicMarket,
  ShareBindings,
  TunnelConfig,
} from "@/lib/api";
import { SHARE_APP_TYPES } from "@/lib/api";
import {
  createShareSchema,
  type CreateShareFormInput,
  type CreateShareFormValues,
} from "@/lib/schemas/share";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogDescription,
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
import { Checkbox } from "@/components/ui/checkbox";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { SHARE_REGIONS } from "@/config/shareRegions";
import {
  DEFAULT_PARALLEL_LIMIT,
  MIN_PARALLEL_LIMIT,
  UNLIMITED_PARALLEL_LIMIT,
  UNLIMITED_TOKEN_LIMIT,
  isUnlimitedParallelLimit,
  isUnlimitedTokenLimit,
  permanentExpiresInSecs,
} from "@/utils/shareUtils";
import { cn } from "@/lib/utils";

export interface CreateShareExtras {
  sharedWithEmails: string[];
  marketAccessMode: "selected" | "all";
}

/**
 * 表单里 Provider 选择器展示的最小 provider 形态。调用方按 `appType` 过滤后传入。
 *
 * Why: share ↔ provider 严格 1:1。一个 provider 同时只能被一个非 deleted share
 * 绑定，所以选择器要把已被其他 share 绑定的 provider 标灰禁选。
 */
export interface ProviderOption {
  id: string;
  name: string;
  /** true 表示该 provider 已被其他 active share 绑定，本表单要禁选 */
  disabled: boolean;
}

interface CreateShareDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 触发对话框时所在的 app tab。用于预填该 app slot 的默认 provider 选择。 */
  defaultApp?: AppId;
  ownerEmail?: string | null;
  tunnelConfig: TunnelConfig;
  tunnelConfigSaving: boolean;
  isSubmitting: boolean;
  submitLabel?: string;
  markets?: PublicMarket[];
  /** P8 多 app share：每个 app_type 各自一组候选（已过滤掉被别的 share 绑定的）。 */
  providersByApp: Record<keyof ShareBindings, ProviderOption[]>;
  onSaveTunnelConfig: (config: TunnelConfig) => Promise<void> | void;
  onSubmit: (
    params: CreateShareParams,
    extras: CreateShareExtras,
  ) => Promise<void> | void;
}

const EXPIRY_PRESETS = [
  { labelKey: "share.expiry.oneHour", value: 3600 },
  { labelKey: "share.expiry.sixHours", value: 6 * 3600 },
  { labelKey: "share.expiry.oneDay", value: 24 * 3600 },
  { labelKey: "share.expiry.sevenDays", value: 7 * 24 * 3600 },
  { labelKey: "share.expiry.thirtyDays", value: 30 * 24 * 3600 },
];

const TOKEN_PRESETS = [10000, 50000, 100000, 500000];
const DEFAULT_TOKEN_LIMIT_FALLBACK = 100000;
const SUBDOMAIN_PREFIX_LENGTH = 5;
const SUBDOMAIN_TIMESTAMP_LENGTH = 5;
const EMPTY_MARKETS: PublicMarket[] = [];

/**
 * 由 owner email 派生默认 subdomain。
 *
 * 形态：`{email-prefix}-{base36-timestamp-suffix}` 例如 `alice-2lr8q`。
 *
 * 单 share 模式时这个函数只取邮箱前缀（同一 owner 创建多个 share 时会撞），
 * 多 share 改造后追加毫秒时间戳的 base36 末 5 位作为去重后缀：
 *  - 同设备连续创建：Date.now() 必然递增，不会撞。
 *  - 跨设备同毫秒并发：前缀通常不同（不同 owner email）。
 *  - email 完全没有可用字母时（如 `123@x.com`）回退到 `s` 占位前缀。
 */
export function deriveSubdomainFromEmail(
  email: string | null | undefined,
): string {
  const local = (email ?? "").split("@")[0] ?? "";
  const filtered = local.toLowerCase().replace(/[^a-z]/g, "");
  const prefix =
    filtered.length === 0 ? "s" : filtered.slice(0, SUBDOMAIN_PREFIX_LENGTH);
  const suffix = Date.now().toString(36).slice(-SUBDOMAIN_TIMESTAMP_LENGTH);
  return `${prefix}-${suffix}`;
}

function buildDefaultValues(
  ownerEmail: string,
  defaultApp: AppId | undefined,
  providersByApp: Record<keyof ShareBindings, ProviderOption[]> | undefined,
): CreateShareFormInput {
  // 当前 app tab 决定默认聚焦的 slot；同时预填一个未被占用的 provider，
  // 减少新用户必填两步的摩擦。其它 slot 默认为空。providersByApp 在测试桩里
  // 可能未传，要 defensive 处理。
  const initialBindings: { claude: string; codex: string; gemini: string } = {
    claude: "",
    codex: "",
    gemini: "",
  };
  const focusApp = toShareAppType(defaultApp);
  const focusCandidates = providersByApp?.[focusApp] ?? [];
  const focusDefault = focusCandidates.find((p) => !p.disabled)?.id ?? "";
  initialBindings[focusApp] = focusDefault;
  return {
    bindings: initialBindings,
    description: "",
    forSale: "Yes",
    autoStart: true,
    tokenLimit: UNLIMITED_TOKEN_LIMIT,
    parallelLimit: UNLIMITED_PARALLEL_LIMIT,
    expiresInSecs: permanentExpiresInSecs(),
    subdomain: deriveSubdomainFromEmail(ownerEmail),
    marketAccessMode: "all",
  };
}

export function CreateShareDialog({
  open,
  onOpenChange,
  defaultApp,
  ownerEmail,
  tunnelConfig,
  tunnelConfigSaving,
  isSubmitting,
  submitLabel,
  markets = EMPTY_MARKETS,
  providersByApp,
  onSaveTunnelConfig,
  onSubmit,
}: CreateShareDialogProps) {
  const { t } = useTranslation();
  const [confirmFreeOpen, setConfirmFreeOpen] = useState(false);
  const [defaultsConfirmOpen, setDefaultsConfirmOpen] = useState(false);
  const [isPermanent, setIsPermanent] = useState(true);
  const [ownerEmailInput, setOwnerEmailInput] = useState(ownerEmail ?? "");
  const [routerDomain, setRouterDomain] = useState(tunnelConfig.domain);
  const [advancedExpanded, setAdvancedExpanded] = useState(false);
  const [advancedOpened, setAdvancedOpened] = useState(false);
  const [lastFiniteTokenLimit, setLastFiniteTokenLimit] = useState(
    DEFAULT_TOKEN_LIMIT_FALLBACK,
  );
  const [lastFiniteParallelLimit, setLastFiniteParallelLimit] = useState(
    DEFAULT_PARALLEL_LIMIT,
  );
  const subdomainManualRef = useRef(false);

  const form = useForm<CreateShareFormInput, unknown, CreateShareFormValues>({
    resolver: zodResolver(createShareSchema),
    defaultValues: buildDefaultValues(
      ownerEmail ?? "",
      defaultApp,
      providersByApp,
    ),
  });

  useEffect(() => {
    if (!open) return;
    setOwnerEmailInput(ownerEmail ?? "");
    setRouterDomain(tunnelConfig.domain);
    form.reset(
      buildDefaultValues(ownerEmail ?? "", defaultApp, providersByApp),
    );
    setIsPermanent(true);
    setLastFiniteTokenLimit(DEFAULT_TOKEN_LIMIT_FALLBACK);
    setLastFiniteParallelLimit(DEFAULT_PARALLEL_LIMIT);
    setAdvancedExpanded(false);
    setAdvancedOpened(false);
    setDefaultsConfirmOpen(false);
    subdomainManualRef.current = false;
    // providersByApp 引用变化（fetch 完成）时也重置一次，确保默认 slot 能选上 provider。
  }, [form, open, ownerEmail, tunnelConfig.domain, defaultApp, providersByApp]);

  useEffect(() => {
    if (!open || subdomainManualRef.current) return;
    const derived = deriveSubdomainFromEmail(ownerEmailInput);
    form.setValue("subdomain", derived, { shouldValidate: false });
  }, [form, open, ownerEmailInput]);

  const tokenLimit = form.watch("tokenLimit") as number;
  const parallelLimit = form.watch("parallelLimit") as number;
  const marketAccessMode = form.watch("marketAccessMode") as "selected" | "all";
  const subdomainValue = form.watch("subdomain") as string;
  const forSaleValue = form.watch("forSale") as "Yes" | "No" | "Free";
  const autoStartValue = form.watch("autoStart") as boolean;
  const descriptionValue = form.watch("description") as string;
  const expiresInSecsValue = form.watch("expiresInSecs") as number;
  const unlimitedTokenLimit = isUnlimitedTokenLimit(tokenLimit);
  const unlimitedParallelLimit = isUnlimitedParallelLimit(parallelLimit);
  const tokenLimitField = form.register("tokenLimit", { valueAsNumber: true });
  const parallelLimitField = form.register("parallelLimit", {
    valueAsNumber: true,
  });
  const subdomainField = form.register("subdomain");
  const normalizedOwnerEmail = ownerEmailInput.trim().toLowerCase();
  const ownerEmailInvalid =
    !normalizedOwnerEmail ||
    !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(normalizedOwnerEmail);
  const defaultShareApp = toShareAppType(defaultApp);
  const defaultProviderId =
    (form.watch(`bindings.${defaultShareApp}` as const) as
      | string
      | undefined) ?? "";
  const defaultProvider = (providersByApp?.[defaultShareApp] ?? []).find(
    (provider) => provider.id === defaultProviderId,
  );
  const defaultProviderLabel =
    defaultProvider?.name ||
    defaultProviderId ||
    t("share.unbound", { defaultValue: "未绑定" });

  const expandAdvanced = () => {
    setAdvancedExpanded(true);
    setAdvancedOpened(true);
  };

  const performSubmit = form.handleSubmit(async (values) => {
    if (ownerEmailInvalid) {
      return;
    }
    const nextRouterDomain = routerDomain.trim();
    if (nextRouterDomain && nextRouterDomain !== tunnelConfig.domain) {
      await onSaveTunnelConfig({ domain: nextRouterDomain });
    }
    await onSubmit(
      {
        ownerEmail: normalizedOwnerEmail,
        // P8：把空字符串过滤掉，只发用户真的选了的 slot。
        bindings: Object.fromEntries(
          (
            Object.entries(values.bindings ?? {}) as Array<
              [keyof ShareBindings, string]
            >
          ).filter(([, pid]) => pid && pid.length > 0),
        ),
        description: values.description || undefined,
        forSale: values.forSale,
        tokenLimit: values.tokenLimit,
        parallelLimit: values.parallelLimit,
        expiresInSecs: values.expiresInSecs,
        subdomain: values.subdomain || undefined,
        autoStart: values.autoStart,
      },
      {
        sharedWithEmails: [],
        marketAccessMode: values.marketAccessMode,
      },
    );
  });

  const handleCreateClick = () => {
    if (ownerEmailInvalid || isSubmitting || tunnelConfigSaving) return;
    if (advancedOpened) {
      void performSubmit();
      return;
    }
    setDefaultsConfirmOpen(true);
  };

  const handleDefaultsConfirmAccept = () => {
    setDefaultsConfirmOpen(false);
    void performSubmit();
  };

  const summary = useMemo(
    () =>
      buildDefaultsSummary(t, {
        autoStart: autoStartValue,
        forSale: forSaleValue,
        marketAccessMode,
        expiresInSecs: expiresInSecsValue,
        isPermanent,
        tokenLimit,
        parallelLimit,
        subdomain: subdomainValue,
        providerBinding: `${defaultShareApp} · ${defaultProviderLabel}`,
      }),
    [
      t,
      autoStartValue,
      forSaleValue,
      marketAccessMode,
      expiresInSecsValue,
      isPermanent,
      tokenLimit,
      parallelLimit,
      subdomainValue,
      defaultShareApp,
      defaultProviderLabel,
    ],
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl overflow-hidden p-0">
        <DialogHeader className="px-5 pb-2 pt-5">
          <DialogTitle className="flex items-center gap-2">
            {t("share.create")}
            {/*
              多 app share 模式：badge 显示当前进入的 app tab 作为"默认聚焦的 slot"，
              用户可在表单里继续勾选其它 slot。
            */}
            <Badge variant="outline" className="capitalize">
              {toShareAppType(defaultApp)}
            </Badge>
          </DialogTitle>
          <DialogDescription className="text-xs">
            {t("share.createDescription")}
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 space-y-4 overflow-y-auto px-5 py-3">
          <div className="space-y-1.5">
            <Label htmlFor="share-create-router">
              {t("share.tunnel.region")}
            </Label>
            <Select value={routerDomain} onValueChange={setRouterDomain}>
              <SelectTrigger id="share-create-router">
                <SelectValue placeholder={t("share.tunnel.selectRegion")} />
              </SelectTrigger>
              <SelectContent>
                {SHARE_REGIONS.map((region) => (
                  <SelectItem key={region.baseUrl} value={region.baseUrl}>
                    {region.region} - {region.baseUrl}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <div className="text-xs text-muted-foreground">
              {t("share.createRouterHint", {
                defaultValue:
                  "创建前选择路由节点。创建完成后当前 share 会绑定到该节点。",
              })}
            </div>
          </div>

          <div className="space-y-1.5">
            <Label>
              {t("share.providerBindings", { defaultValue: "Provider 绑定" })}
            </Label>
            <div className="text-xs text-muted-foreground">
              {t("share.providerBindingsHint", {
                defaultValue:
                  "默认绑定当前 App 选中的 Provider。需要让同一个 share 支持多个 App 时，再进入高级设置独立配置。",
              })}
            </div>
            <div className="rounded-md border border-default/50 bg-muted/10 p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="min-w-0">
                  <div className="text-xs font-medium uppercase text-muted-foreground">
                    {defaultShareApp}
                  </div>
                  <div className="mt-1 truncate text-sm font-medium">
                    {defaultProviderLabel}
                  </div>
                </div>
                {defaultProviderId ? (
                  <Badge variant="outline" className="text-[10px]">
                    {t("share.bound", { defaultValue: "已绑定" })}
                  </Badge>
                ) : (
                  <Badge
                    variant="outline"
                    className="text-[10px] text-muted-foreground"
                  >
                    {t("share.unbound", { defaultValue: "未绑定" })}
                  </Badge>
                )}
              </div>
              {!defaultProviderId ? (
                <div className="mt-2 text-xs text-destructive">
                  {t("share.providerBindingEmptyCurrent", {
                    defaultValue:
                      "当前 App 没有可自动绑定的 Provider。请先添加 Provider，或进入高级设置选择其它 App。",
                  })}
                </div>
              ) : null}
            </div>
            {form.formState.errors.bindings ? (
              <div className="text-xs text-destructive">
                {t(
                  (form.formState.errors.bindings as { message?: string })
                    ?.message ?? "share.validation.providerRequired",
                  { defaultValue: "至少为一个 app 选择 provider" },
                )}
              </div>
            ) : null}
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="share-owner-email">
              {t("share.ownerEmail", { defaultValue: "Owner Email" })}
            </Label>
            <Input
              id="share-owner-email"
              type="email"
              value={ownerEmailInput}
              onChange={(event) => setOwnerEmailInput(event.target.value)}
              placeholder="owner@example.com"
            />
            <div className="text-xs text-muted-foreground">
              {t("share.ownerEmailCreateHint", {
                defaultValue:
                  "该邮箱会作为 share owner 上报到 router。router 页面使用相同邮箱登录后可查看 API Key 和编辑设置。",
              })}
            </div>
            <FieldError
              error={
                ownerEmailInput.trim() && ownerEmailInvalid
                  ? t("share.validation.invalidEmail", {
                      defaultValue: "邮箱格式无效",
                    })
                  : undefined
              }
            />
          </div>

          <div className="space-y-1.5">
            <Label htmlFor="share-description">{t("share.description")}</Label>
            <Textarea
              id="share-description"
              className="min-h-[72px]"
              maxLength={200}
              placeholder={t("share.descriptionPlaceholder")}
              {...form.register("description")}
            />
            <div className="text-xs text-muted-foreground">
              {t("share.descriptionHint")}
            </div>
            <FieldError error={form.formState.errors.description?.message} />
          </div>

          <div className="rounded-lg border border-border-default bg-muted/10">
            <button
              type="button"
              className="flex w-full items-center justify-between gap-2 px-3 py-2 text-sm font-medium"
              onClick={() =>
                advancedExpanded ? setAdvancedExpanded(false) : expandAdvanced()
              }
              aria-expanded={advancedExpanded}
              aria-controls="share-create-advanced"
            >
              <span className="flex items-center gap-2">
                {advancedExpanded ? (
                  <ChevronDown className="h-4 w-4" />
                ) : (
                  <ChevronRight className="h-4 w-4" />
                )}
                {t("share.createDialog.advancedToggle", {
                  defaultValue: "高级设置",
                })}
              </span>
              <span className="text-xs text-muted-foreground">
                {t("share.createDialog.advancedHint", {
                  defaultValue: "未展开则使用默认值，点击创建会弹出二次确认",
                })}
              </span>
            </button>
            {advancedExpanded ? (
              <div
                id="share-create-advanced"
                className="grid gap-3 border-t border-border-default px-3 py-3 md:grid-cols-2"
              >
                <div className="space-y-2 md:col-span-2">
                  <div>
                    <Label>
                      {t("share.providerBindingsAdvanced", {
                        defaultValue: "按 App 独立绑定 Provider",
                      })}
                    </Label>
                    <div className="mt-1 text-xs text-muted-foreground">
                      {t("share.providerBindingsAdvancedHint", {
                        defaultValue:
                          "每个 App 最多绑定一个 Provider；留空表示该 App 在本 share 上不可用。",
                      })}
                    </div>
                  </div>
                  <div className="grid gap-2">
                    {SHARE_APP_TYPES.map((app) => {
                      const candidates = providersByApp?.[app] ?? [];
                      const fieldKey = `bindings.${app}` as const;
                      const value =
                        (form.watch(fieldKey) as string | undefined) ?? "";
                      return (
                        <div
                          key={app}
                          className="grid gap-1 rounded-md border border-default/50 p-2"
                        >
                          <div className="flex items-center justify-between text-xs font-medium uppercase text-muted-foreground">
                            <span>{app}</span>
                            {value ? (
                              <Badge variant="outline" className="text-[10px]">
                                {t("share.bound", { defaultValue: "已绑定" })}
                              </Badge>
                            ) : (
                              <Badge
                                variant="outline"
                                className="text-[10px] text-muted-foreground"
                              >
                                {t("share.unbound", { defaultValue: "未绑定" })}
                              </Badge>
                            )}
                          </div>
                          <div className="flex items-center gap-2">
                            <Select
                              value={value || undefined}
                              onValueChange={(next) =>
                                form.setValue(fieldKey, next, {
                                  shouldValidate: true,
                                  shouldDirty: true,
                                })
                              }
                            >
                              <SelectTrigger
                                id={`share-create-provider-${app}`}
                                className="flex-1"
                              >
                                <SelectValue
                                  placeholder={t(
                                    "share.providerBindingPlaceholder",
                                    {
                                      defaultValue: `为 ${app} 选一个 provider`,
                                    },
                                  )}
                                />
                              </SelectTrigger>
                              <SelectContent>
                                {candidates.length === 0 ? (
                                  <SelectItem value="__empty__" disabled>
                                    {t("share.providerBindingEmpty", {
                                      defaultValue: `{{app}} 下没有可绑定的 provider`,
                                      app,
                                    })}
                                  </SelectItem>
                                ) : (
                                  candidates.map((provider) => (
                                    <SelectItem
                                      key={provider.id}
                                      value={provider.id}
                                      disabled={provider.disabled}
                                    >
                                      {provider.name}
                                      {provider.disabled
                                        ? ` · ${t(
                                            "share.providerBindingTaken",
                                            {
                                              defaultValue:
                                                "已被其他 share 绑定",
                                            },
                                          )}`
                                        : ""}
                                    </SelectItem>
                                  ))
                                )}
                              </SelectContent>
                            </Select>
                            {value ? (
                              <Button
                                type="button"
                                variant="ghost"
                                size="sm"
                                onClick={() =>
                                  form.setValue(fieldKey, "", {
                                    shouldValidate: true,
                                    shouldDirty: true,
                                  })
                                }
                                title={t("share.providerBindingClear", {
                                  defaultValue: "清空（解绑）",
                                })}
                              >
                                <X className="h-4 w-4" />
                              </Button>
                            ) : null}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="share-for-sale">{t("share.forSale")}</Label>
                  <Select
                    value={forSaleValue}
                    onValueChange={(value) => {
                      const next = value as "Yes" | "No" | "Free";
                      if (next === "Free") {
                        setConfirmFreeOpen(true);
                      } else {
                        form.setValue("forSale", next, {
                          shouldDirty: true,
                          shouldValidate: true,
                        });
                      }
                    }}
                  >
                    <SelectTrigger id="share-for-sale">
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
                  <FieldError error={form.formState.errors.forSale?.message} />
                </div>

                <div className="space-y-1.5">
                  <div className="flex min-h-10 items-center gap-2 rounded-md border border-border-default px-3 py-2">
                    <Checkbox
                      id="share-auto-start"
                      checked={autoStartValue}
                      onCheckedChange={(checked) =>
                        form.setValue("autoStart", checked === true, {
                          shouldDirty: true,
                          shouldValidate: true,
                        })
                      }
                    />
                    <Label
                      htmlFor="share-auto-start"
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.autoStart")}
                    </Label>
                  </div>
                  <div className="text-xs text-muted-foreground">
                    {t("share.autoStartHint")}
                  </div>
                </div>

                <div className="space-y-1.5 md:col-span-2">
                  <Label>
                    {t("share.market.title", { defaultValue: "Market" })}
                  </Label>
                  <div className="flex flex-wrap items-center gap-2">
                    <Select
                      value={
                        marketAccessMode === "all" ? "__all__" : "__selected__"
                      }
                      onValueChange={(value) => {
                        form.setValue(
                          "marketAccessMode",
                          value === "__all__" ? "all" : "selected",
                          { shouldDirty: true, shouldValidate: true },
                        );
                      }}
                    >
                      <SelectTrigger className="w-48">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="__all__">
                          {t("share.market.all", { defaultValue: "全部" })}
                        </SelectItem>
                        <SelectItem value="__selected__">
                          {t("share.market.none", {
                            defaultValue: "不授权 (默认)",
                          })}
                        </SelectItem>
                      </SelectContent>
                    </Select>
                    {marketAccessMode === "all" ? (
                      <Badge variant="secondary" className="text-xs">
                        {t("share.market.allSelected", {
                          defaultValue: "已选中所有 Market",
                        })}
                      </Badge>
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {t("share.market.default", {
                          defaultValue: "默认，不授权 Market",
                        })}
                      </span>
                    )}
                  </div>
                  {markets.length > 0 ? (
                    <div className="text-xs text-muted-foreground">
                      {t("share.createDialog.marketHint", {
                        defaultValue:
                          "创建后可在 share 卡片中精细调整 Market 列表。",
                      })}
                    </div>
                  ) : null}
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="share-expires">{t("share.expiresIn")}</Label>
                  <Input
                    id="share-expires"
                    type="number"
                    disabled={isPermanent}
                    {...form.register("expiresInSecs", { valueAsNumber: true })}
                  />
                  <div className="flex flex-wrap gap-1.5">
                    {EXPIRY_PRESETS.map((preset) => (
                      <Button
                        key={preset.value}
                        type="button"
                        variant="outline"
                        size="sm"
                        className="h-7 px-2 text-xs"
                        disabled={isPermanent}
                        onClick={() =>
                          form.setValue("expiresInSecs", preset.value)
                        }
                      >
                        {t(preset.labelKey)}
                      </Button>
                    ))}
                  </div>
                  <div className="flex items-center gap-2 pt-1">
                    <Checkbox
                      id="share-expires-permanent"
                      checked={isPermanent}
                      onCheckedChange={(checked) => {
                        const next = checked === true;
                        setIsPermanent(next);
                        if (next) {
                          form.setValue(
                            "expiresInSecs",
                            permanentExpiresInSecs(),
                            { shouldValidate: true },
                          );
                        } else {
                          form.setValue("expiresInSecs", 24 * 3600, {
                            shouldValidate: true,
                          });
                        }
                      }}
                    />
                    <Label
                      htmlFor="share-expires-permanent"
                      className="cursor-pointer text-sm font-normal"
                    >
                      {t("share.expiry.permanent")}
                    </Label>
                  </div>
                  <FieldError
                    error={form.formState.errors.expiresInSecs?.message}
                  />
                </div>

                <div className="space-y-1.5">
                  <div className="flex items-center justify-between gap-3">
                    <Label htmlFor="share-token-limit">
                      {t("share.tokenLimit")}
                    </Label>
                    <div className="flex items-center gap-2">
                      <Checkbox
                        id="share-token-limit-unlimited"
                        checked={unlimitedTokenLimit}
                        onCheckedChange={(checked) => {
                          const next = checked === true;
                          if (next) {
                            if (
                              typeof tokenLimit === "number" &&
                              tokenLimit > 0
                            ) {
                              setLastFiniteTokenLimit(tokenLimit);
                            }
                            form.setValue("tokenLimit", UNLIMITED_TOKEN_LIMIT, {
                              shouldDirty: true,
                              shouldValidate: true,
                            });
                            return;
                          }
                          form.setValue("tokenLimit", lastFiniteTokenLimit, {
                            shouldDirty: true,
                            shouldValidate: true,
                          });
                        }}
                      />
                      <Label
                        htmlFor="share-token-limit-unlimited"
                        className="cursor-pointer text-sm font-normal"
                      >
                        {t("share.unlimited")}
                      </Label>
                    </div>
                  </div>
                  <Input
                    id="share-token-limit"
                    type="number"
                    disabled={unlimitedTokenLimit}
                    {...tokenLimitField}
                    onChange={(event) => {
                      tokenLimitField.onChange(event);
                      const next = Number.parseInt(event.target.value, 10);
                      if (Number.isFinite(next) && next > 0) {
                        setLastFiniteTokenLimit(next);
                      }
                    }}
                  />
                  <div className="flex flex-wrap gap-2">
                    {TOKEN_PRESETS.map((preset) => (
                      <Button
                        key={preset}
                        type="button"
                        variant="outline"
                        size="sm"
                        className="h-7 px-2 text-xs"
                        disabled={unlimitedTokenLimit}
                        onClick={() => {
                          setLastFiniteTokenLimit(preset);
                          form.setValue("tokenLimit", preset, {
                            shouldDirty: true,
                            shouldValidate: true,
                          });
                        }}
                      >
                        {preset.toLocaleString()}
                      </Button>
                    ))}
                  </div>
                  <FieldError
                    error={form.formState.errors.tokenLimit?.message}
                  />
                </div>

                <div className="space-y-1.5">
                  <div className="flex items-center justify-between gap-3">
                    <Label htmlFor="share-parallel-limit">
                      {t("share.parallelLimit")}
                    </Label>
                    <div className="flex items-center gap-2">
                      <Checkbox
                        id="share-parallel-limit-unlimited"
                        checked={unlimitedParallelLimit}
                        onCheckedChange={(checked) => {
                          const next = checked === true;
                          if (next) {
                            if (
                              typeof parallelLimit === "number" &&
                              parallelLimit >= MIN_PARALLEL_LIMIT
                            ) {
                              setLastFiniteParallelLimit(parallelLimit);
                            }
                            form.setValue(
                              "parallelLimit",
                              UNLIMITED_PARALLEL_LIMIT,
                              { shouldDirty: true, shouldValidate: true },
                            );
                            return;
                          }
                          form.setValue(
                            "parallelLimit",
                            lastFiniteParallelLimit,
                            { shouldDirty: true, shouldValidate: true },
                          );
                        }}
                      />
                      <Label
                        htmlFor="share-parallel-limit-unlimited"
                        className="cursor-pointer text-sm font-normal"
                      >
                        {t("share.unlimited")}
                      </Label>
                    </div>
                  </div>
                  <Input
                    id="share-parallel-limit"
                    type="number"
                    min={MIN_PARALLEL_LIMIT}
                    disabled={unlimitedParallelLimit}
                    {...parallelLimitField}
                    onChange={(event) => {
                      parallelLimitField.onChange(event);
                      const next = Number.parseInt(event.target.value, 10);
                      if (Number.isFinite(next) && next >= MIN_PARALLEL_LIMIT) {
                        setLastFiniteParallelLimit(next);
                      }
                    }}
                  />
                  <div className="text-xs text-muted-foreground">
                    {t("share.parallelLimitHint")}
                  </div>
                  <FieldError
                    error={form.formState.errors.parallelLimit?.message}
                  />
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="share-subdomain">
                    {t("share.subdomain")}
                  </Label>
                  <Input
                    id="share-subdomain"
                    placeholder="my-share"
                    {...subdomainField}
                    onChange={(event) => {
                      subdomainField.onChange(event);
                      subdomainManualRef.current = true;
                    }}
                  />
                  <div className="text-xs text-muted-foreground">
                    {t("share.subdomainHint")}
                  </div>
                  <FieldError
                    error={form.formState.errors.subdomain?.message}
                  />
                </div>
              </div>
            ) : null}
          </div>

          {!advancedExpanded ? (
            <div className="rounded-md border border-dashed border-border-default bg-muted/10 px-3 py-2 text-xs text-muted-foreground">
              <div className="font-medium">
                {t("share.createDialog.summaryHeading", {
                  defaultValue: "将以默认设置创建：",
                })}
              </div>
              <ul className="mt-1 grid gap-0.5 md:grid-cols-2">
                {summary.map((line) => (
                  <li key={line.key}>
                    <span className="text-muted-foreground">
                      {line.label}：
                    </span>
                    <span>{line.value}</span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {descriptionValue.length > 200 ? null : null}
        </div>

        <DialogFooter className="px-5 py-4">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.cancel")}
          </Button>
          <Button
            onClick={handleCreateClick}
            disabled={isSubmitting || tunnelConfigSaving || ownerEmailInvalid}
          >
            {submitLabel ?? t("share.create")}
          </Button>
        </DialogFooter>
      </DialogContent>

      <ConfirmDialog
        isOpen={confirmFreeOpen}
        title={t("share.forSaleFreeConfirmTitle")}
        message={t("share.forSaleFreeConfirmMessage")}
        variant="destructive"
        zIndex="top"
        onConfirm={() => {
          form.setValue("forSale", "Free", {
            shouldDirty: true,
            shouldValidate: true,
          });
          setConfirmFreeOpen(false);
        }}
        onCancel={() => setConfirmFreeOpen(false)}
      />

      <ConfirmDialog
        isOpen={defaultsConfirmOpen}
        title={t("share.createDialog.defaultsConfirm.title", {
          defaultValue: "确认使用默认设置创建？",
        })}
        message={[
          t("share.createDialog.defaultsConfirm.body", {
            defaultValue: '你未展开 "高级设置"，将按以下默认值创建：',
          }),
          "",
          ...summary.map((line) => `• ${line.label}：${line.value}`),
        ].join("\n")}
        confirmText={t("share.createDialog.defaultsConfirm.confirm", {
          defaultValue: "确认创建",
        })}
        cancelText={t("share.createDialog.defaultsConfirm.cancel", {
          defaultValue: "返回修改",
        })}
        variant="info"
        zIndex="top"
        onConfirm={handleDefaultsConfirmAccept}
        onCancel={() => setDefaultsConfirmOpen(false)}
      />
    </Dialog>
  );
}

function toShareAppType(app?: AppId): "claude" | "codex" | "gemini" {
  if (app === "codex" || app === "gemini") return app;
  return "claude";
}

function FieldError({ error }: { error?: string }) {
  if (!error) return null;
  return <p className={cn("text-sm text-destructive")}>{error}</p>;
}

interface SummaryLine {
  key: string;
  label: string;
  value: string;
}

function buildDefaultsSummary(
  t: ReturnType<typeof useTranslation>["t"],
  values: {
    autoStart: boolean;
    forSale: "Yes" | "No" | "Free";
    marketAccessMode: "selected" | "all";
    expiresInSecs: number;
    isPermanent: boolean;
    tokenLimit: number;
    parallelLimit: number;
    subdomain: string;
    providerBinding: string;
  },
): SummaryLine[] {
  const lines: SummaryLine[] = [
    {
      key: "providerBinding",
      label: t("share.providerBindings", { defaultValue: "Provider 绑定" }),
      value: values.providerBinding,
    },
    {
      key: "autoStart",
      label: t("share.autoStart"),
      value: values.autoStart
        ? t("common.enabled", { defaultValue: "已启用" })
        : t("common.disabled", { defaultValue: "未启用" }),
    },
    {
      key: "forSale",
      label: t("share.forSale"),
      value: t(`share.forSaleOptions.${values.forSale.toLowerCase()}`),
    },
    {
      key: "market",
      label: t("share.market.title", { defaultValue: "Market" }),
      value:
        values.marketAccessMode === "all"
          ? t("share.market.allSelected", { defaultValue: "已选中所有 Market" })
          : t("share.market.default", { defaultValue: "默认，不授权 Market" }),
    },
    {
      key: "expiry",
      label: t("share.expiresAt"),
      value: values.isPermanent
        ? t("share.expiry.permanentLabel", { defaultValue: "永久" })
        : t("share.createDialog.summary.expiresInSecs", {
            defaultValue: "{{seconds}} 秒",
            seconds: values.expiresInSecs,
          }),
    },
    {
      key: "tokenLimit",
      label: t("share.tokenLimit"),
      value: isUnlimitedTokenLimit(values.tokenLimit)
        ? t("share.unlimited", { defaultValue: "无上限" })
        : values.tokenLimit.toLocaleString(),
    },
    {
      key: "parallelLimit",
      label: t("share.parallelLimit"),
      value: isUnlimitedParallelLimit(values.parallelLimit)
        ? t("share.unlimited", { defaultValue: "无上限" })
        : String(values.parallelLimit),
    },
    {
      key: "subdomain",
      label: t("share.subdomain"),
      value: values.subdomain.trim()
        ? values.subdomain.trim()
        : t("share.createDialog.subdomainAuto", {
            defaultValue: "(由后端生成)",
          }),
    },
  ];
  return lines;
}
