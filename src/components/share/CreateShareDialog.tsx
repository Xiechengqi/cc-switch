import { useEffect, useState } from "react";
import { zodResolver } from "@hookform/resolvers/zod";
import { useForm } from "react-hook-form";
import { useTranslation } from "react-i18next";
import type { AppId, CreateShareParams } from "@/lib/api";
import {
  createShareSchema,
  type CreateShareFormInput,
  type CreateShareFormValues,
} from "@/lib/schemas/share";
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
import {
  DEFAULT_PARALLEL_LIMIT,
  MIN_PARALLEL_LIMIT,
  UNLIMITED_PARALLEL_LIMIT,
  UNLIMITED_TOKEN_LIMIT,
  isUnlimitedParallelLimit,
  isUnlimitedTokenLimit,
  permanentExpiresInSecs,
} from "@/utils/shareUtils";

interface CreateShareDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaultApp?: AppId;
  ownerEmail?: string | null;
  isSubmitting: boolean;
  submitLabel?: string;
  onSubmit: (params: CreateShareParams) => Promise<void> | void;
}

const EXPIRY_PRESETS = [
  { labelKey: "share.expiry.oneHour", value: 3600 },
  { labelKey: "share.expiry.sixHours", value: 6 * 3600 },
  { labelKey: "share.expiry.oneDay", value: 24 * 3600 },
  { labelKey: "share.expiry.sevenDays", value: 7 * 24 * 3600 },
  { labelKey: "share.expiry.thirtyDays", value: 30 * 24 * 3600 },
];

const TOKEN_PRESETS = [10000, 50000, 100000, 500000];
const DEFAULT_TOKEN_LIMIT = 100000;

export function CreateShareDialog({
  open,
  onOpenChange,
  defaultApp,
  ownerEmail,
  isSubmitting,
  submitLabel,
  onSubmit,
}: CreateShareDialogProps) {
  const { t } = useTranslation();
  const [confirmFreeOpen, setConfirmFreeOpen] = useState(false);
  const [isPermanent, setIsPermanent] = useState(false);
  const [lastFiniteTokenLimit, setLastFiniteTokenLimit] =
    useState(DEFAULT_TOKEN_LIMIT);
  const [lastFiniteParallelLimit, setLastFiniteParallelLimit] = useState(
    DEFAULT_PARALLEL_LIMIT,
  );

  const form = useForm<CreateShareFormInput, unknown, CreateShareFormValues>({
    resolver: zodResolver(createShareSchema),
    defaultValues: {
      description: "",
      forSale: "No",
      tokenLimit: DEFAULT_TOKEN_LIMIT,
      parallelLimit: DEFAULT_PARALLEL_LIMIT,
      expiresInSecs: 24 * 3600,
      apiKey: "",
      subdomain: "",
    },
  });

  useEffect(() => {
    if (!open) return;
    form.reset({
      description: "",
      forSale: "No",
      tokenLimit: DEFAULT_TOKEN_LIMIT,
      parallelLimit: DEFAULT_PARALLEL_LIMIT,
      expiresInSecs: 24 * 3600,
      apiKey: "",
      subdomain: "",
    });
    setIsPermanent(false);
    setLastFiniteTokenLimit(DEFAULT_TOKEN_LIMIT);
    setLastFiniteParallelLimit(DEFAULT_PARALLEL_LIMIT);
  }, [form, open]);

  const tokenLimit = form.watch("tokenLimit") as number;
  const parallelLimit = form.watch("parallelLimit") as number;
  const unlimitedTokenLimit = isUnlimitedTokenLimit(tokenLimit);
  const unlimitedParallelLimit = isUnlimitedParallelLimit(parallelLimit);
  const tokenLimitField = form.register("tokenLimit", { valueAsNumber: true });
  const parallelLimitField = form.register("parallelLimit", {
    valueAsNumber: true,
  });

  const submit = form.handleSubmit(async (values) => {
    await onSubmit({
      appType: toShareAppType(defaultApp),
      description: values.description || undefined,
      forSale: values.forSale,
      tokenLimit: values.tokenLimit,
      parallelLimit: values.parallelLimit,
      expiresInSecs: values.expiresInSecs,
      apiKey: values.apiKey || undefined,
      subdomain: values.subdomain || undefined,
    });
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl overflow-hidden p-0">
        <DialogHeader>
          <DialogTitle>{t("share.create")}</DialogTitle>
          <DialogDescription>{t("share.createDescription")}</DialogDescription>
        </DialogHeader>

        <div className="flex-1 space-y-5 overflow-y-auto px-6 py-5">
          <div className="space-y-2">
            <Label htmlFor="share-owner-email">
              {t("share.ownerEmail", { defaultValue: "Owner Email" })}
            </Label>
            <Input
              id="share-owner-email"
              value={ownerEmail ?? ""}
              readOnly
              disabled
            />
            <div className="text-xs text-muted-foreground">
              {t("share.ownerEmailCreateHint", {
                defaultValue:
                  "Share 名称会自动使用当前登录邮箱，创建后当前设备不能切换到其他邮箱。",
              })}
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="share-description">{t("share.description")}</Label>
            <Textarea
              id="share-description"
              maxLength={200}
              placeholder={t("share.descriptionPlaceholder")}
              {...form.register("description")}
            />
            <div className="text-xs text-muted-foreground">
              {t("share.descriptionHint")}
            </div>
            <FieldError error={form.formState.errors.description?.message} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="share-for-sale">{t("share.forSale")}</Label>
            <Select
              value={form.watch("forSale")}
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
          </div>

          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
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
                        if (typeof tokenLimit === "number" && tokenLimit > 0) {
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
              <FieldError error={form.formState.errors.tokenLimit?.message} />
            </div>

            <div className="space-y-2">
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
                          {
                            shouldDirty: true,
                            shouldValidate: true,
                          },
                        );
                        return;
                      }
                      form.setValue("parallelLimit", lastFiniteParallelLimit, {
                        shouldDirty: true,
                        shouldValidate: true,
                      });
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

            <div className="space-y-2">
              <Label htmlFor="share-expires">{t("share.expiresIn")}</Label>
              <Input
                id="share-expires"
                type="number"
                disabled={isPermanent}
                {...form.register("expiresInSecs", { valueAsNumber: true })}
              />
              <div className="flex flex-wrap gap-2">
                {EXPIRY_PRESETS.map((preset) => (
                  <Button
                    key={preset.value}
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={isPermanent}
                    onClick={() => form.setValue("expiresInSecs", preset.value)}
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
                      form.setValue("expiresInSecs", permanentExpiresInSecs(), {
                        shouldValidate: true,
                      });
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
          </div>

          <div className="space-y-2">
            <Label htmlFor="share-api-key">{t("share.apiKey")}</Label>
            <Input
              id="share-api-key"
              placeholder="custom-share-key"
              {...form.register("apiKey")}
            />
            <div className="text-xs text-muted-foreground">
              {t("share.apiKeyHint")}
            </div>
            <FieldError error={form.formState.errors.apiKey?.message} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="share-subdomain">{t("share.subdomain")}</Label>
            <Input
              id="share-subdomain"
              placeholder="my-share"
              {...form.register("subdomain")}
            />
            <div className="text-xs text-muted-foreground">
              {t("share.subdomainHint")}
            </div>
            <FieldError error={form.formState.errors.subdomain?.message} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="share-notes">{t("share.createHelp")}</Label>
            <Textarea id="share-notes" value={t("share.createHint")} readOnly />
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t("common.cancel")}
          </Button>
          <Button onClick={() => void submit()} disabled={isSubmitting}>
            {submitLabel ?? t("share.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function toShareAppType(app?: AppId): "claude" | "codex" | "gemini" {
  if (app === "codex" || app === "gemini") return app;
  return "claude";
}

function FieldError({ error }: { error?: string }) {
  if (!error) return null;
  return <p className="text-sm text-destructive">{error}</p>;
}
