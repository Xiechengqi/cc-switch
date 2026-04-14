import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff, RotateCcw, Save } from "lucide-react";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { ShareStatusBadge } from "./ShareStatusBadge";
import {
  formatUtcDateTime,
  getShareTunnelRuntimeStatus,
  getShareUsageRatio,
  maskSensitive,
  resolveShareTunnelInfo,
} from "@/utils/shareUtils";

interface ShareDetailDrawerProps {
  share: ShareRecord | null;
  tunnelStatus?: TunnelInfo | null;
  tunnelConfig: TunnelConfig;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onResetUsage: (share: ShareRecord) => void;
  onUpdateTokenLimit: (share: ShareRecord, tokenLimit: number) => void;
  onUpdateSubdomain: (share: ShareRecord, subdomain: string) => void;
  onUpdateApiKey: (share: ShareRecord, apiKey: string) => void;
  onUpdateExpiration: (share: ShareRecord, expiresAt: string) => void;
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
  onUpdateSubdomain,
  onUpdateApiKey,
  onUpdateExpiration,
  busy = false,
}: ShareDetailDrawerProps) {
  const { t } = useTranslation();
  const [revealToken, setRevealToken] = useState(false);
  const [tokenLimitInput, setTokenLimitInput] = useState("");
  const [subdomainInput, setSubdomainInput] = useState("");
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [expiryDateInput, setExpiryDateInput] = useState("");
  const [expiryHourInput, setExpiryHourInput] = useState("");
  const [expiryMinuteInput, setExpiryMinuteInput] = useState("");

  useEffect(() => {
    setTokenLimitInput(share ? String(share.tokenLimit) : "");
    setSubdomainInput(share?.subdomain ?? "");
    setApiKeyInput(share?.shareToken ?? "");
    if (share?.expiresAt) {
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
      setExpiryDateInput("");
      setExpiryHourInput("");
      setExpiryMinuteInput("");
    }
  }, [share]);

  if (!share) return null;

  const ratio = getShareUsageRatio(share);
  const tunnelDisplay = resolveShareTunnelInfo(share, tunnelConfig);
  const tunnelRuntimeStatus = getShareTunnelRuntimeStatus(share, tunnelStatus);
  const parsedTokenLimit = Number.parseInt(tokenLimitInput, 10);
  const tokenLimitDirty =
    Number.isFinite(parsedTokenLimit) && parsedTokenLimit !== share.tokenLimit;
  const tokenLimitInvalid =
    tokenLimitInput.trim().length === 0 ||
    !Number.isFinite(parsedTokenLimit) ||
    parsedTokenLimit <= 0;
  const subdomainDirty = subdomainInput.trim() !== (share.subdomain ?? "");
  const subdomainInvalid =
    subdomainInput.trim().length < 3 ||
    !/^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])?$/.test(subdomainInput.trim()) ||
    ["admin", "api", "www", "cdn-cgi"].includes(subdomainInput.trim());
  const apiKeyDirty = apiKeyInput.trim() !== share.shareToken;
  const apiKeyInvalid =
    apiKeyInput.trim().length < 8 ||
    !/^[A-Za-z0-9._-]{8,128}$/.test(apiKeyInput.trim());
  const parsedExpiryHour = Number.parseInt(expiryHourInput, 10);
  const parsedExpiryMinute = Number.parseInt(expiryMinuteInput, 10);
  const expiryIso =
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
  const expiryDirty = expiryIso && expiryIso !== share.expiresAt;
  const expiryInvalid =
    !expiryDateInput ||
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
            <ShareStatusBadge status={share.status} />
          </DialogTitle>
          <DialogDescription>{t("share.editDescription")}</DialogDescription>
        </DialogHeader>
        <div className="flex-1 space-y-6 overflow-y-auto px-6 py-6">
          <section className="grid gap-4 md:grid-cols-2">
            <InfoField label={t("share.id")} value={share.id} />
            <InfoField
              label={t("share.apiKey")}
              value={revealToken ? share.shareToken : maskSensitive(share.shareToken)}
              action={
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => setRevealToken((prev) => !prev)}
                >
                  {revealToken ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </Button>
              }
            />
            <InfoField
              label={t("share.requestsCount")}
              value={String(share.requestsCount)}
            />
            <InfoField
              label={t("share.lastUsedAt")}
              value={share.lastUsedAt ? formatUtcDateTime(share.lastUsedAt) : t("share.never")}
            />
            <InfoField
              label={t("share.createdAt")}
              value={formatUtcDateTime(share.createdAt)}
            />
            <InfoField
              label={t("share.expiresAt")}
              value={formatUtcDateTime(share.expiresAt)}
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
              value={tunnelStatus?.remotePort ? String(tunnelStatus.remotePort) : "-"}
            />
            <InfoField
              label={t("share.tunnelHealth")}
              value={t(`share.statuses.${tunnelRuntimeStatus}`)}
            />
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">{t("share.expirationSettings")}</div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_120px_120px_auto]">
              <div className="space-y-2">
                <div className="text-sm font-medium">{t("share.expirationDate")}</div>
                <Input
                  type="date"
                  value={expiryDateInput}
                  disabled={busy}
                  onChange={(event) => setExpiryDateInput(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <div className="text-sm font-medium">{t("share.expirationHour")}</div>
                <Input
                  type="number"
                  min={0}
                  max={23}
                  value={expiryHourInput}
                  disabled={busy}
                  onChange={(event) => setExpiryHourInput(event.target.value)}
                />
              </div>
              <div className="space-y-2">
                <div className="text-sm font-medium">{t("share.expirationMinute")}</div>
                <Input
                  type="number"
                  min={0}
                  max={59}
                  value={expiryMinuteInput}
                  disabled={busy}
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
            <div className="text-xs text-muted-foreground">
              {t("share.expirationEditHint")}
            </div>
          </section>

          <section className="space-y-3">
            <div className="text-sm font-medium">{t("share.apiKeySettings")}</div>
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
            <div className="text-sm font-medium">{t("share.subdomainSettings")}</div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <Input
                  value={subdomainInput}
                  disabled={busy}
                  onChange={(event) => setSubdomainInput(event.target.value.toLowerCase())}
                />
                <div className="text-xs text-muted-foreground">
                  {t("share.subdomainEditHint")}
                </div>
              </div>
              <div className="flex items-end">
                <Button
                  variant="outline"
                  disabled={busy || subdomainInvalid || !subdomainDirty}
                  onClick={() => onUpdateSubdomain(share, subdomainInput.trim())}
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
                {share.tokensUsed} / {share.tokenLimit}
              </div>
            </div>
            <div className="h-2 rounded-full bg-muted">
              <div
                className="h-2 rounded-full bg-blue-500"
                style={{ width: `${Math.max(4, ratio * 100)}%` }}
              />
            </div>
            <div className="text-sm font-medium">{t("share.usageActions")}</div>
            <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto]">
              <div className="space-y-2">
                <div className="text-sm font-medium">{t("share.tokenLimit")}</div>
                <Input
                  type="number"
                  min={1}
                  step={1}
                  value={tokenLimitInput}
                  disabled={busy}
                  onChange={(event) => setTokenLimitInput(event.target.value)}
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
