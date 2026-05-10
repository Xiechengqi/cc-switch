import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";

interface ShareRouterBarProps {
  proxyRunning: boolean;
  proxyAddress?: string | null;
  proxyPort?: number | null;
  hasShare: boolean;
  onCreate: () => void;
}

export function ShareRouterBar({
  proxyRunning,
  proxyAddress,
  proxyPort,
  hasShare,
  onCreate,
}: ShareRouterBarProps) {
  const { t } = useTranslation();

  if (hasShare && proxyRunning) {
    return null;
  }

  return (
    <div className="rounded-xl border border-border-default/70 bg-card/80 px-4 py-3">
      <div className="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div className="text-sm text-muted-foreground">
          {hasShare
            ? t("share.routerLockedAfterCreate", {
                defaultValue: "路由节点已绑定到当前 share。",
              })
            : t("share.createDescription")}
        </div>

        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
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
