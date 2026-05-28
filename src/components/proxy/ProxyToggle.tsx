/**
 * Header share switch.
 *
 * This keeps the existing proxy takeover implementation underneath, but the
 * user-facing flow is framed as enabling or disabling share access.
 */

import { useMemo, useState } from "react";
import { Loader2, Share2 } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CreateShareDialog } from "@/components/share/CreateShareDialog";
import {
  shareApi,
  type AppId,
  type CreateShareParams,
  type ShareRecord,
} from "@/lib/api";
import {
  useConfigureTunnelMutation,
  useCreateShareMutation,
  useSettingsQuery,
  useUpdateShareAclMutation,
} from "@/lib/query";
import { shareKeys } from "@/lib/query/share";
import {
  useProxyStatus,
  useProxyTakeoverStatus,
  useSetProxyTakeoverForApp,
  useStartProxyServer,
} from "@/lib/query/proxy";
import { cn } from "@/lib/utils";
import { extractErrorMessage } from "@/utils/errorUtils";
import { getTunnelConfigFromSettings } from "@/utils/shareUtils";

interface ProxyToggleProps {
  className?: string;
  activeApp: AppId;
}

type ShareToggleStage =
  | "idle"
  | "checking"
  | "creating-share"
  | "confirm-start-share"
  | "starting-share"
  | "starting-proxy"
  | "enabling-takeover"
  | "disabling-takeover";

type PendingIntent =
  | { type: "create-and-enable" }
  | { type: "start-and-enable"; shareId: string };

export function ProxyToggle({ className, activeApp }: ProxyToggleProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: settings } = useSettingsQuery();
  const { data: proxyStatus } = useProxyStatus();
  const { data: takeoverStatus } = useProxyTakeoverStatus();
  const createShareMutation = useCreateShareMutation();
  const updateAclMutation = useUpdateShareAclMutation();
  const configureTunnelMutation = useConfigureTunnelMutation();
  const startProxyMutation = useStartProxyServer();
  const setTakeoverMutation = useSetProxyTakeoverForApp();
  const [stage, setStage] = useState<ShareToggleStage>("idle");
  const [createOpen, setCreateOpen] = useState(false);
  const [startTarget, setStartTarget] = useState<ShareRecord | null>(null);
  const [pendingIntent, setPendingIntent] = useState<PendingIntent | null>(
    null,
  );

  const tunnelConfig = useMemo(
    () => getTunnelConfigFromSettings(settings),
    [settings],
  );
  const takeoverEnabled = Boolean(takeoverStatus?.[activeApp]);
  const appLabel = getAppLabel(activeApp);
  const pending =
    stage !== "idle" ||
    createShareMutation.isPending ||
    configureTunnelMutation.isPending ||
    startProxyMutation.isPending ||
    setTakeoverMutation.isPending;

  const fetchShares = async () =>
    queryClient.fetchQuery({
      queryKey: shareKeys.list(),
      queryFn: shareApi.list,
    });

  const invalidateShareState = async (shareId?: string) => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: shareKeys.list() }),
      shareId
        ? queryClient.invalidateQueries({ queryKey: shareKeys.detail(shareId) })
        : Promise.resolve(),
      shareId
        ? queryClient.invalidateQueries({
            queryKey: shareKeys.tunnelStatus(shareId),
          })
        : Promise.resolve(),
      shareId
        ? queryClient.invalidateQueries({
            queryKey: shareKeys.connectInfo(shareId),
          })
        : Promise.resolve(),
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] }),
      queryClient.invalidateQueries({ queryKey: ["proxyTakeoverStatus"] }),
    ]);
  };

  const ensureProxyRunning = async () => {
    if (proxyStatus?.running) return;
    setStage("starting-proxy");
    await startProxyMutation.mutateAsync();
  };

  const enableTakeover = async () => {
    setStage("enabling-takeover");
    await setTakeoverMutation.mutateAsync({
      appType: activeApp,
      enabled: true,
    });
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] }),
      queryClient.invalidateQueries({ queryKey: ["proxyTakeoverStatus"] }),
    ]);
    toast.success(
      t("share.toggle.enabled", {
        defaultValue: "分享已开启",
      }),
      { closeButton: true },
    );
  };

  const disableTakeover = async () => {
    setStage("disabling-takeover");
    await setTakeoverMutation.mutateAsync({
      appType: activeApp,
      enabled: false,
    });
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] }),
      queryClient.invalidateQueries({ queryKey: ["proxyTakeoverStatus"] }),
    ]);
    toast.success(
      t("share.toggle.disabled", {
        defaultValue: "分享已关闭",
      }),
      { closeButton: true },
    );
    setStage("idle");
  };

  const startShareAndEnable = async (share: ShareRecord) => {
    try {
      setStage("starting-share");
      await shareApi.enable(share.id);
      await invalidateShareState(share.id);
      await ensureProxyRunning();
      await enableTakeover();
      setPendingIntent(null);
      setStage("idle");
    } catch (error) {
      toast.error(
        t("share.toggle.enableFailed", {
          defaultValue: "开启分享失败：{{error}}",
          error: extractErrorMessage(error),
        }),
      );
      throw error;
    } finally {
      setStage("idle");
    }
  };

  const createAndEnable = async (
    params: CreateShareParams,
    extras: { sharedWithEmails: string[]; marketAccessMode: "selected" | "all" },
  ) => {
    try {
      setStage("creating-share");
      const created = await createShareMutation.mutateAsync(params);
      if (
        extras.marketAccessMode === "all" ||
        extras.sharedWithEmails.length > 0
      ) {
        await updateAclMutation.mutateAsync({
          shareId: created.id,
          sharedWithEmails: extras.sharedWithEmails,
          marketAccessMode: extras.marketAccessMode,
        });
      }
      setCreateOpen(false);
      await startShareAndEnable(created);
    } finally {
      setStage("idle");
    }
  };

  const handleEnable = async () => {
    let flowContinuesInDialog = false;
    setStage("checking");
    try {
      const shares = await fetchShares();
      const share = selectBestShare(shares, activeApp);
      if (!share) {
        flowContinuesInDialog = true;
        setPendingIntent({ type: "create-and-enable" });
        setCreateOpen(true);
        setStage("creating-share");
        return;
      }

      if (!isShareRunning(share)) {
        flowContinuesInDialog = true;
        setPendingIntent({ type: "start-and-enable", shareId: share.id });
        setStartTarget(share);
        setStage("confirm-start-share");
        return;
      }

      await ensureProxyRunning();
      await enableTakeover();
      setStage("idle");
    } catch (error) {
      toast.error(
        t("share.toggle.enableFailed", {
          defaultValue: "开启分享失败：{{error}}",
          error: extractErrorMessage(error),
        }),
      );
    } finally {
      if (!flowContinuesInDialog) {
        setStage("idle");
      }
    }
  };

  const handleToggle = async (checked: boolean) => {
    if (pending) return;
    try {
      if (!checked) {
        await disableTakeover();
        return;
      }
      await handleEnable();
    } catch (error) {
      toast.error(
        checked
          ? t("share.toggle.enableFailed", {
              defaultValue: "开启分享失败：{{error}}",
              error: extractErrorMessage(error),
            })
          : t("share.toggle.disableFailed", {
              defaultValue: "关闭分享失败：{{error}}",
              error: extractErrorMessage(error),
            }),
      );
    }
  };

  const tooltipText = takeoverEnabled
    ? t("share.toggle.tooltipActive", {
        app: appLabel,
        defaultValue: "{{app}} 分享已开启，点击关闭",
      })
    : t("share.toggle.tooltipInactive", {
        app: appLabel,
        defaultValue: "开启 {{app}} 分享",
      });

  return (
    <>
      <div
        className={cn(
          "flex items-center gap-1 px-1.5 h-8 rounded-lg bg-muted/50 transition-all",
          className,
        )}
        title={tooltipText}
        aria-label={
          takeoverEnabled
            ? t("share.toggle.disable", { defaultValue: "关闭分享" })
            : t("share.toggle.enable", { defaultValue: "开启分享" })
        }
      >
        {pending ? (
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
        ) : (
          <Share2
            className={cn(
              "h-4 w-4 transition-colors",
              takeoverEnabled
                ? "text-emerald-500 animate-pulse"
                : "text-muted-foreground",
            )}
          />
        )}
        <Switch
          aria-label={
            takeoverEnabled
              ? t("share.toggle.disable", { defaultValue: "关闭分享" })
              : t("share.toggle.enable", { defaultValue: "开启分享" })
          }
          checked={takeoverEnabled}
          onCheckedChange={(checked) => void handleToggle(checked)}
          disabled={pending}
        />
      </div>

      <CreateShareDialog
        open={createOpen}
        onOpenChange={(open) => {
          setCreateOpen(open);
          if (!open) {
            if (pendingIntent?.type === "create-and-enable") {
              setPendingIntent(null);
            }
            setStage("idle");
          }
        }}
        ownerEmail={null}
        defaultApp={activeApp}
        isSubmitting={
          createShareMutation.isPending || stage === "creating-share"
        }
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        submitLabel={t("share.toggle.createAndEnable", {
          defaultValue: "创建并开启分享",
        })}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
        onSubmit={createAndEnable}
      />

      <ConfirmDialog
        isOpen={Boolean(startTarget)}
        variant="info"
        title={t("share.toggle.startTitle", {
          defaultValue: "启动分享",
        })}
        message={t("share.toggle.startDescription", {
          defaultValue: "当前分享尚未启动，启动后即可对外访问。",
        })}
        confirmText={t("share.toggle.startAndEnable", {
          defaultValue: "启动并开启分享",
        })}
        onCancel={() => {
          setStartTarget(null);
          setPendingIntent(null);
          setStage("idle");
        }}
        onConfirm={() => {
          const share = startTarget;
          setStartTarget(null);
          if (!share) return;
          void startShareAndEnable(share).catch(() => undefined);
        }}
      />
    </>
  );
}

function getAppLabel(app: AppId) {
  if (app === "claude") return "Claude";
  if (app === "codex") return "Codex";
  if (app === "gemini") return "Gemini";
  return app;
}

function isShareRunning(share: ShareRecord) {
  return share.status === "active" && Boolean(share.tunnelUrl);
}

function selectBestShare(shares: ShareRecord[], activeApp: AppId) {
  return (
    shares.find(
      (share) => share.appType === activeApp && isShareRunning(share),
    ) ??
    shares.find((share) => isShareRunning(share)) ??
    shares.find((share) => share.appType === activeApp) ??
    shares[0] ??
    null
  );
}
