import { useTranslation } from "react-i18next";
import type { TunnelConfig } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SHARE_REGIONS } from "@/config/shareRegions";

interface ShareRouterBarProps {
  tunnelConfig: TunnelConfig;
  tunnelConfigSaving: boolean;
  proxyRunning: boolean;
  proxyAddress?: string | null;
  proxyPort?: number | null;
  hasShare: boolean;
  ownerEmail?: string | null;
  ownerAuthenticated?: boolean;
  onCreate: () => void;
  onChangeOwner?: () => void;
  onVerifyOwner?: () => void;
  onSaveTunnelConfig: (config: TunnelConfig) => Promise<void> | void;
}

export function ShareRouterBar({
  tunnelConfig,
  tunnelConfigSaving,
  proxyRunning,
  proxyAddress,
  proxyPort,
  hasShare,
  ownerEmail,
  ownerAuthenticated = false,
  onCreate,
  onChangeOwner,
  onVerifyOwner,
  onSaveTunnelConfig,
}: ShareRouterBarProps) {
  const { t } = useTranslation();

  const handleRegionChange = (value: string) => {
    if (value !== tunnelConfig.domain) {
      void onSaveTunnelConfig({ domain: value });
    }
  };

  return (
    <div className="rounded-xl border border-border-default/70 bg-card/80 px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <Label className="text-sm text-muted-foreground">
            {t("share.tunnel.region")}
          </Label>
          <Select
            value={tunnelConfig.domain}
            onValueChange={handleRegionChange}
            disabled={tunnelConfigSaving}
          >
            <SelectTrigger className="w-full sm:w-[260px]">
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
        </div>

        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {hasShare && ownerEmail && ownerAuthenticated ? (
            <Button
              type="button"
              variant="outline"
              onClick={onChangeOwner}
              className="w-full sm:w-auto"
            >
              {t("share.ownerChange.action", {
                defaultValue: "Change Owner Email",
              })}
            </Button>
          ) : null}
          {hasShare && ownerEmail && !ownerAuthenticated ? (
            <Button
              type="button"
              variant="outline"
              onClick={onVerifyOwner}
              className="w-full sm:w-auto"
            >
              {t("share.ownerLogin.reverifyAction", {
                defaultValue: "重新验证 Owner 邮箱",
              })}
            </Button>
          ) : null}
          {!hasShare ? (
            <Button onClick={onCreate} className="w-full sm:w-auto">
              {t("share.create")}
            </Button>
          ) : null}
        </div>
      </div>

      {!proxyRunning ? (
        <div className="mt-3 text-xs text-amber-600 dark:text-amber-400">
          {t("share.proxyCompactWarning", {
            address: proxyAddress || "127.0.0.1",
            port: proxyPort || 53000,
          })}
        </div>
      ) : null}
    </div>
  );
}
