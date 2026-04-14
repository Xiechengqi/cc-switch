import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Save, Share2 } from "lucide-react";
import type { ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import { tunnelConfigSchema } from "@/lib/schemas/share";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { ShareStatusBadge } from "./ShareStatusBadge";
import { getShareTunnelRuntimeStatus } from "@/utils/shareUtils";

interface ShareHeroProps {
  tunnelConfigured: boolean;
  tunnelConfig: TunnelConfig;
  tunnelConfigSaving: boolean;
  proxyRunning: boolean;
  proxyAddress?: string | null;
  proxyPort?: number | null;
  share: ShareRecord | null;
  tunnelStatus?: TunnelInfo | null;
  hasShare: boolean;
  onCreate: () => void;
  onSaveTunnelConfig: (config: TunnelConfig) => Promise<void> | void;
}

export function ShareHero({
  tunnelConfigured,
  tunnelConfig,
  tunnelConfigSaving,
  proxyRunning,
  proxyAddress,
  proxyPort,
  share,
  tunnelStatus,
  hasShare,
  onCreate,
  onSaveTunnelConfig,
}: ShareHeroProps) {
  const { t } = useTranslation();
  const [domain, setDomain] = useState(tunnelConfig.domain);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDomain(tunnelConfig.domain);
  }, [tunnelConfig.domain]);

  const tunnelRuntimeStatus = share
    ? getShareTunnelRuntimeStatus(share, tunnelStatus)
    : null;
  const isDirty = domain !== tunnelConfig.domain;

  const handleSave = async () => {
    const result = tunnelConfigSchema.safeParse({ domain });
    if (!result.success) {
      setError(t(result.error.issues[0]?.message || "share.validation.required"));
      return;
    }
    setError(null);
    await onSaveTunnelConfig(result.data);
  };

  return (
    <Card className="overflow-hidden border-border-default/70 bg-gradient-to-br from-sky-500/10 via-background to-emerald-500/10">
      <CardContent className="space-y-5 px-6 py-6">
        <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
          <div className="space-y-3">
            <div className="inline-flex h-11 w-11 items-center justify-center rounded-2xl bg-sky-500/15 text-sky-600 dark:text-sky-300">
              <Share2 className="h-5 w-5" />
            </div>
            <div>
              <h2 className="text-2xl font-semibold tracking-tight">
                {t("share.title")}
              </h2>
              <p className="mt-2 max-w-3xl text-sm leading-6 text-muted-foreground">
                {t("share.subtitle")}
              </p>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <ShareStatusBadge
                kind="tunnel"
                status={tunnelConfigured ? "running" : "stopped"}
              />
              <span className="text-sm text-muted-foreground">
                {tunnelConfigured
                  ? t("share.tunnel.configured")
                  : t("share.tunnel.notConfigured")}
              </span>
              {share ? <ShareStatusBadge status={share.status} /> : null}
              {share ? (
                <span className="text-sm text-muted-foreground">
                  {t(`share.statuses.${tunnelRuntimeStatus ?? "unknown"}`)}
                </span>
              ) : null}
            </div>
          </div>

          {!hasShare ? <Button onClick={onCreate}>{t("share.create")}</Button> : null}
        </div>

        {!proxyRunning ? (
          <div className="rounded-2xl border border-amber-300/70 bg-amber-50 px-4 py-4 text-amber-950">
            <div className="text-sm font-semibold">{t("share.proxyNotRunningTitle")}</div>
            <div className="mt-1 text-sm leading-6 text-amber-900/80">
              {t("share.proxyNotRunningDescription", {
                address: proxyAddress || "127.0.0.1",
                port: proxyPort || 3000,
              })}
            </div>
          </div>
        ) : null}

        <div className="grid gap-3 rounded-2xl border border-border-default/70 bg-background/70 p-4 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-end">
          <div className="space-y-2">
            <div className="text-sm font-medium">{t("share.tunnel.domain")}</div>
            <Input
              placeholder="example.com"
              value={domain}
              onChange={(event) => setDomain(event.target.value)}
            />
            <div className="text-xs text-muted-foreground">
              {t("share.tunnel.description")}
            </div>
            {error ? <div className="text-sm text-red-500">{error}</div> : null}
          </div>
          <Button
            type="button"
            onClick={() => void handleSave()}
            disabled={tunnelConfigSaving || !isDirty}
          >
            <Save className="h-4 w-4" />
            {t("common.save")}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
