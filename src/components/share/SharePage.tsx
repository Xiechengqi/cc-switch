import { useMemo, useState } from "react";
import { useQueries, useQueryClient, type Query } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  shareApi,
  type AppId,
  type ShareRecord,
  type TunnelInfo,
} from "@/lib/api";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { useSettingsQuery } from "@/lib/query";
import { useProxyStatus } from "@/lib/query/proxy";
import {
  useConfigureTunnelMutation,
  useCreateShareMutation,
  useDeleteShareMutation,
  useDisableShareMutation,
  useEnableShareMutation,
  useResetShareUsageMutation,
  useSharesQuery,
  useUpdateShareApiKeyMutation,
  useUpdateShareDescriptionMutation,
  useUpdateShareExpirationMutation,
  useUpdateShareForSaleMutation,
  useUpdateShareSubdomainMutation,
  useUpdateShareTokenLimitMutation,
} from "@/lib/query";
import { shareKeys } from "@/lib/query/share";
import { usageKeys } from "@/lib/query/usage";
import { extractErrorMessage } from "@/utils/errorUtils";
import {
  getTunnelConfigFromSettings,
  isTunnelConfigured,
} from "@/utils/shareUtils";
import { ShareConnectDialog } from "./ShareConnectDialog";
import { CreateShareDialog } from "./CreateShareDialog";
import { ShareDetailDrawer } from "./ShareDetailDrawer";
import { ShareList } from "./ShareList";
import { ShareRouterBar } from "./ShareRouterBar";

interface SharePageProps {
  defaultApp?: AppId;
}

export function SharePage(_props: SharePageProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: shares = [], isLoading, error, refetch } = useSharesQuery();
  const { data: settings } = useSettingsQuery();
  const { data: proxyStatus } = useProxyStatus();
  const tunnelConfigured = useMemo(
    () => isTunnelConfigured(settings),
    [settings],
  );
  const tunnelConfig = useMemo(
    () => getTunnelConfigFromSettings(settings),
    [settings],
  );
  const [createOpen, setCreateOpen] = useState(false);
  const [detailShareId, setDetailShareId] = useState<string | null>(null);
  const [connectShareId, setConnectShareId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ShareRecord | null>(null);
  const [pendingActionShareId, setPendingActionShareId] = useState<
    string | null
  >(null);

  const createMutation = useCreateShareMutation();
  const deleteMutation = useDeleteShareMutation();
  const enableMutation = useEnableShareMutation();
  const disableMutation = useDisableShareMutation();
  const resetUsageMutation = useResetShareUsageMutation();
  const updateApiKeyMutation = useUpdateShareApiKeyMutation();
  const updateDescriptionMutation = useUpdateShareDescriptionMutation();
  const updateForSaleMutation = useUpdateShareForSaleMutation();
  const updateExpirationMutation = useUpdateShareExpirationMutation();
  const updateSubdomainMutation = useUpdateShareSubdomainMutation();
  const updateTokenLimitMutation = useUpdateShareTokenLimitMutation();
  const configureTunnelMutation = useConfigureTunnelMutation();

  const tunnelQueries = useQueries({
    queries: shares.map((share) => ({
      queryKey: shareKeys.tunnelStatus(share.id),
      queryFn: () => shareApi.getTunnelStatus(share.id),
      enabled: share.status === "active" && Boolean(share.tunnelUrl),
      refetchInterval: (query: Query<TunnelInfo | null, Error>) =>
        share.status === "active" && Boolean(query.state.data) ? 8000 : false,
      refetchIntervalInBackground: true,
    })),
  });

  const tunnelStatusMap = useMemo(
    () =>
      Object.fromEntries(
        shares.map((share, index) => [
          share.id,
          tunnelQueries[index]?.data ?? null,
        ]),
      ),
    [shares, tunnelQueries],
  );

  const detailShare =
    shares.find((share) => share.id === detailShareId) ?? null;
  const connectShare =
    shares.find((share) => share.id === connectShareId) ?? null;
  const primaryShare = shares[0] ?? null;

  const handleCreate = async (
    params: Parameters<typeof createMutation.mutateAsync>[0],
  ) => {
    await createMutation.mutateAsync(params);
    setCreateOpen(false);
  };

  const runShareAction = async (
    share: ShareRecord,
    action: () => Promise<unknown>,
  ) => {
    setPendingActionShareId(share.id);
    try {
      await action();
    } finally {
      setPendingActionShareId(null);
    }
  };

  const handleRefresh = async (share: ShareRecord) => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: shareKeys.list() }),
      queryClient.invalidateQueries({
        queryKey: shareKeys.tunnelStatus(share.id),
      }),
      queryClient.invalidateQueries({ queryKey: shareKeys.detail(share.id) }),
      queryClient.invalidateQueries({
        queryKey: usageKeys
          .logs({ preset: "7d", shareId: share.id }, 0, 10)
          .slice(0, 2),
      }),
    ]);
  };

  return (
    <div className="px-6 py-4">
      <div className="mx-auto flex max-w-7xl flex-col gap-5 pb-10">
        <ShareRouterBar
          tunnelConfig={tunnelConfig}
          tunnelConfigSaving={configureTunnelMutation.isPending}
          proxyRunning={proxyStatus?.running ?? false}
          proxyAddress={proxyStatus?.address ?? null}
          proxyPort={proxyStatus?.port ?? null}
          hasShare={shares.length > 0}
          onCreate={() => setCreateOpen(true)}
          onSaveTunnelConfig={(config) =>
            configureTunnelMutation.mutateAsync(config)
          }
        />

        <ShareList
          shares={primaryShare ? [primaryShare] : []}
          tunnelStatusMap={tunnelStatusMap}
          tunnelConfig={tunnelConfig}
          tunnelConfigured={tunnelConfigured}
          isLoading={isLoading}
          error={error ? extractErrorMessage(error) : null}
          pendingAction={pendingActionShareId}
          onRetry={() => void refetch()}
          onCreate={() => setCreateOpen(true)}
          onOpenDetail={(share) => setDetailShareId(share.id)}
          onOpenConnect={(share) => setConnectShareId(share.id)}
          onDelete={(share) => setDeleteTarget(share)}
          onEnable={(share) =>
            void runShareAction(share, () =>
              enableMutation.mutateAsync(share.id),
            )
          }
          onDisable={(share) =>
            void runShareAction(share, () =>
              disableMutation.mutateAsync(share.id),
            )
          }
          onRefresh={(share) => void handleRefresh(share)}
        />
      </div>

      <CreateShareDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        isSubmitting={createMutation.isPending}
        onSubmit={handleCreate}
      />

      <ShareDetailDrawer
        share={detailShare}
        tunnelStatus={detailShare ? tunnelStatusMap[detailShare.id] : null}
        tunnelConfig={tunnelConfig}
        open={Boolean(detailShare)}
        onOpenChange={(open) => {
          if (!open) setDetailShareId(null);
        }}
        onResetUsage={(share) =>
          void runShareAction(share, () =>
            resetUsageMutation.mutateAsync(share.id),
          )
        }
        onUpdateSubdomain={(share, subdomain) =>
          void runShareAction(share, () =>
            updateSubdomainMutation.mutateAsync({
              shareId: share.id,
              subdomain,
            }),
          )
        }
        onUpdateApiKey={(share, apiKey) =>
          void runShareAction(share, () =>
            updateApiKeyMutation.mutateAsync({ shareId: share.id, apiKey }),
          )
        }
        onUpdateDescription={(share, description) =>
          void runShareAction(share, () =>
            updateDescriptionMutation.mutateAsync({
              shareId: share.id,
              description,
            }),
          )
        }
        onUpdateForSale={(share, forSale) =>
          void runShareAction(share, () =>
            updateForSaleMutation.mutateAsync({
              shareId: share.id,
              forSale,
            }),
          )
        }
        onUpdateExpiration={(share, expiresAt) =>
          void runShareAction(share, () =>
            updateExpirationMutation.mutateAsync({
              shareId: share.id,
              expiresAt,
            }),
          )
        }
        onUpdateTokenLimit={(share, tokenLimit) =>
          void runShareAction(share, () =>
            updateTokenLimitMutation.mutateAsync({
              shareId: share.id,
              tokenLimit,
            }),
          )
        }
        busy={pendingActionShareId === detailShare?.id}
      />

      <ShareConnectDialog
        share={connectShare}
        tunnelConfig={tunnelConfig}
        open={Boolean(connectShare)}
        onOpenChange={(open) => {
          if (!open) setConnectShareId(null);
        }}
      />

      <ConfirmDialog
        isOpen={Boolean(deleteTarget)}
        title={t("share.confirmDeleteTitle")}
        message={t("share.confirmDeleteMessage", {
          name: deleteTarget?.name ?? "",
        })}
        onCancel={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget) return;
          void runShareAction(deleteTarget, async () => {
            await deleteMutation.mutateAsync(deleteTarget.id);
            setDeleteTarget(null);
            if (detailShareId === deleteTarget.id) setDetailShareId(null);
            if (connectShareId === deleteTarget.id) setConnectShareId(null);
          });
        }}
      />
    </div>
  );
}
