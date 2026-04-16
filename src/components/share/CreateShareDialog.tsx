import { useEffect } from "react";
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

interface CreateShareDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaultApp?: AppId;
  isSubmitting: boolean;
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

export function CreateShareDialog({
  open,
  onOpenChange,
  isSubmitting,
  onSubmit,
}: CreateShareDialogProps) {
  const { t } = useTranslation();

  const form = useForm<CreateShareFormInput, unknown, CreateShareFormValues>({
    resolver: zodResolver(createShareSchema),
    defaultValues: {
      name: "Proxy Share",
      description: "",
      forSale: "No",
      tokenLimit: 100000,
      expiresInSecs: 24 * 3600,
      apiKey: "",
      subdomain: "",
    },
  });

  useEffect(() => {
    if (!open) return;
    form.reset({
      name: "Proxy Share",
      description: "",
      forSale: "No",
      tokenLimit: 100000,
      expiresInSecs: 24 * 3600,
      apiKey: "",
      subdomain: "",
    });
  }, [form, open]);

  const submit = form.handleSubmit(async (values) => {
    await onSubmit({
      name: values.name,
      description: values.description || undefined,
      forSale: values.forSale,
      tokenLimit: values.tokenLimit,
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
            <Label htmlFor="share-name">{t("share.name")}</Label>
            <Input id="share-name" {...form.register("name")} />
            <FieldError error={form.formState.errors.name?.message} />
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
              onValueChange={(value) =>
                form.setValue("forSale", value as "Yes" | "No", {
                  shouldDirty: true,
                  shouldValidate: true,
                })
              }
            >
              <SelectTrigger id="share-for-sale">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="No">{t("share.forSaleOptions.no")}</SelectItem>
                <SelectItem value="Yes">{t("share.forSaleOptions.yes")}</SelectItem>
              </SelectContent>
            </Select>
            <div className="text-xs text-muted-foreground">
              {t("share.forSaleHint")}
            </div>
            <FieldError error={form.formState.errors.forSale?.message} />
          </div>

          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="share-token-limit">{t("share.tokenLimit")}</Label>
              <Input
                id="share-token-limit"
                type="number"
                {...form.register("tokenLimit", { valueAsNumber: true })}
              />
              <div className="flex flex-wrap gap-2">
                {TOKEN_PRESETS.map((preset) => (
                  <Button
                    key={preset}
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => form.setValue("tokenLimit", preset)}
                  >
                    {preset.toLocaleString()}
                  </Button>
                ))}
              </div>
              <FieldError error={form.formState.errors.tokenLimit?.message} />
            </div>

            <div className="space-y-2">
              <Label htmlFor="share-expires">{t("share.expiresIn")}</Label>
              <Input
                id="share-expires"
                type="number"
                {...form.register("expiresInSecs", { valueAsNumber: true })}
              />
              <div className="flex flex-wrap gap-2">
                {EXPIRY_PRESETS.map((preset) => (
                  <Button
                    key={preset.value}
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => form.setValue("expiresInSecs", preset.value)}
                  >
                    {t(preset.labelKey)}
                  </Button>
                ))}
              </div>
              <FieldError error={form.formState.errors.expiresInSecs?.message} />
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
            {t("share.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function FieldError({ error }: { error?: string }) {
  if (!error) return null;
  return <p className="text-sm text-destructive">{error}</p>;
}
