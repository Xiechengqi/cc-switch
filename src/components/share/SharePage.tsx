import { useMemo, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  shareApi,
  type AppId,
  type ShareRecord,
  type ShareTunnelStatus,
} from "@/lib/api";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { useSettingsQuery } from "@/lib/query";
import { useProxyStatus } from "@/lib/query/proxy";
import {
  useConfigureTunnelMutation,
  useCreateShareMutation,
  useDeleteShareMutation,
  useShareMarketsQuery,
  useDisableShareMutation,
  useEnableShareMutation,
  useProvidersQuery,
  useResetShareUsageMutation,
  useUpdateShareAclMutation,
  useUpdateShareAutoStartMutation,
  useSharesQuery,
  useUpdateShareDescriptionMutation,
  useUpdateShareExpirationMutation,
  useUpdateShareForSaleMutation,
  useUpdateShareForSaleOfficialPricePercentMutation,
  useUpdateShareOwnerEmailMutation,
  useUpdateShareParallelLimitMutation,
  useUpdateShareSubdomainMutation,
  useUpdateShareTokenLimitMutation,
} from "@/lib/query";
import { shareKeys } from "@/lib/query/share";
import { extractErrorMessage } from "@/utils/errorUtils";
import {
  getTunnelConfigFromSettings,
  isTunnelConfigured,
} from "@/utils/shareUtils";
import { CreateShareDialog } from "./CreateShareDialog";
import { ShareList } from "./ShareList";
import { ShareRouterBar } from "./ShareRouterBar";
import type { Provider } from "@/types";

const SHARE_PROVIDER_APPS = [
  { app: "claude", label: "Claude" },
  { app: "codex", label: "Codex" },
  { app: "gemini", label: "Gemini" },
] as const;

interface SharePageProps {
  defaultApp?: AppId;
}

export function SharePage({ defaultApp }: SharePageProps) {
  const { t } = useTranslation();
  const { data: shares = [], isLoading, error, refetch } = useSharesQuery();
  const { data: settings } = useSettingsQuery();
  const { data: proxyStatus } = useProxyStatus();
  const claudeProvidersQuery = useProvidersQuery("claude");
  const codexProvidersQuery = useProvidersQuery("codex");
  const geminiProvidersQuery = useProvidersQuery("gemini");
  const tunnelConfigured = useMemo(
    () => isTunnelConfigured(settings),
    [settings],
  );
  const tunnelConfig = useMemo(
    () => getTunnelConfigFromSettings(settings),
    [settings],
  );
  const [createOpen, setCreateOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ShareRecord | null>(null);
  const [pendingActionShareId, setPendingActionShareId] = useState<
    string | null
  >(null);

  const createMutation = useCreateShareMutation();
  const deleteMutation = useDeleteShareMutation();
  const enableMutation = useEnableShareMutation();
  const disableMutation = useDisableShareMutation();
  const resetUsageMutation = useResetShareUsageMutation();
  const updateDescriptionMutation = useUpdateShareDescriptionMutation();
  const updateForSaleMutation = useUpdateShareForSaleMutation();
  const updateSharePricingMutation =
    useUpdateShareForSaleOfficialPricePercentMutation();
  const updateExpirationMutation = useUpdateShareExpirationMutation();
  const updateAutoStartMutation = useUpdateShareAutoStartMutation();
  const updateOwnerEmailMutation = useUpdateShareOwnerEmailMutation();
  const updateAclMutation = useUpdateShareAclMutation();
  const updateParallelLimitMutation = useUpdateShareParallelLimitMutation();
  const updateSubdomainMutation = useUpdateShareSubdomainMutation();
  const updateTokenLimitMutation = useUpdateShareTokenLimitMutation();
  const configureTunnelMutation = useConfigureTunnelMutation();
  const {
    data: markets = [],
    isLoading: marketsLoading,
    error: marketsError,
    refetch: refetchMarkets,
  } = useShareMarketsQuery(Boolean(tunnelConfig.domain));
  const providerQueries = useMemo(
    () => ({
      claude: claudeProvidersQuery.data,
      codex: codexProvidersQuery.data,
      gemini: geminiProvidersQuery.data,
    }),
    [
      claudeProvidersQuery.data,
      codexProvidersQuery.data,
      geminiProvidersQuery.data,
    ],
  );
  const providerSalePricing = useMemo(
    () =>
      SHARE_PROVIDER_APPS.map(({ app, label }) => {
        const data = providerQueries[app];
        const provider: Provider | undefined =
          data?.providers?.[data.currentProviderId];
        return {
          app,
          label,
          providerName: provider?.name,
          percent: provider?.meta?.forSaleOfficialPricePercent,
        };
      }),
    [providerQueries],
  );

  const tunnelQueries = useQueries({
    queries: shares.map((share) => ({
      queryKey: shareKeys.tunnelStatus(share.id),
      queryFn: () => shareApi.getTunnelStatus(share.id),
      enabled: share.status === "active",
      refetchInterval: share.status === "active" ? 8000 : false,
      refetchIntervalInBackground: true,
    })),
  });

  const tunnelRuntimeStatusMap = useMemo<
    Record<string, ShareTunnelStatus | null>
  >(
    () =>
      Object.fromEntries(
        shares.map((share, index) => [
          share.id,
          tunnelQueries[index]?.data ?? null,
        ]),
      ),
    [shares, tunnelQueries],
  );

  const tunnelStatusMap = useMemo(
    () =>
      Object.fromEntries(
        shares.map((share) => [
          share.id,
          tunnelRuntimeStatusMap[share.id]?.info ?? null,
        ]),
      ),
    [shares, tunnelRuntimeStatusMap],
  );

  const primaryShare = shares[0] ?? null;

  const handleCreate = async (
    params: Parameters<typeof createMutation.mutateAsync>[0],
    extras: { sharedWithEmails: string[]; marketAccessMode: "selected" | "all" },
  ) => {
    const created = await createMutation.mutateAsync(params);
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
    await runShareAction(created, () => enableMutation.mutateAsync(created.id));
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

  return (
    <div className="px-6 py-4">
      <div className="mx-auto flex max-w-7xl flex-col gap-5 pb-10">
        <ShareRouterBar
          proxyRunning={proxyStatus?.running ?? false}
          proxyAddress={proxyStatus?.address ?? null}
          proxyPort={proxyStatus?.port ?? null}
          hasShare={shares.length > 0}
          onCreate={() => setCreateOpen(true)}
        />

        <ShareList
          shares={primaryShare ? [primaryShare] : []}
          tunnelStatusMap={tunnelStatusMap}
          tunnelConfig={tunnelConfig}
          tunnelConfigured={tunnelConfigured}
          isLoading={isLoading}
          error={error ? extractErrorMessage(error) : null}
          pendingAction={pendingActionShareId}
          markets={markets}
          providerSalePricing={providerSalePricing}
          marketsLoading={marketsLoading}
          marketsError={marketsError ? extractErrorMessage(marketsError) : null}
          onRetryMarkets={() => void refetchMarkets()}
          onRetry={() => void refetch()}
          onCreate={() => setCreateOpen(true)}
          onDelete={(share) => setDeleteTarget(share)}
          onEnable={(share) =>
            void runShareAction(share, () =>
              enableMutation.mutateAsync(share.id),
            ).catch(() => undefined)
          }
          onDisable={(share) =>
            void runShareAction(share, () =>
              disableMutation.mutateAsync(share.id),
            ).catch(() => undefined)
          }
          onResetUsage={(share) =>
            runShareAction(share, () =>
              resetUsageMutation.mutateAsync(share.id),
            )
          }
          onUpdateSubdomain={(share, subdomain) =>
            runShareAction(share, () =>
              updateSubdomainMutation.mutateAsync({
                shareId: share.id,
                subdomain,
              }),
            )
          }
          onUpdateDescription={(share, description) =>
            runShareAction(share, () =>
              updateDescriptionMutation.mutateAsync({
                shareId: share.id,
                description,
              }),
            )
          }
          onUpdateForSale={(share, forSale) =>
            runShareAction(share, () =>
              updateForSaleMutation.mutateAsync({
                shareId: share.id,
                forSale,
              }),
            )
          }
          onUpdateShareSalePricing={(share, pricing) =>
            runShareAction(share, () =>
              updateSharePricingMutation.mutateAsync({
                shareId: share.id,
                pricing,
              }),
            )
          }
          onUpdateExpiration={(share, expiresAt) =>
            runShareAction(share, () =>
              updateExpirationMutation.mutateAsync({
                shareId: share.id,
                expiresAt,
              }),
            )
          }
          onUpdateAutoStart={(share, autoStart) =>
            runShareAction(share, () =>
              updateAutoStartMutation.mutateAsync({
                shareId: share.id,
                autoStart,
              }),
            )
          }
          onUpdateOwnerEmail={(share, ownerEmail) =>
            runShareAction(share, () =>
              updateOwnerEmailMutation.mutateAsync({
                shareId: share.id,
                ownerEmail,
              }),
            )
          }
          onUpdateAcl={(share, sharedWithEmails, marketAccessMode) =>
            runShareAction(share, () =>
              updateAclMutation.mutateAsync({
                shareId: share.id,
                sharedWithEmails,
                marketAccessMode,
              }),
            )
          }
          onUpdateTokenLimit={(share, tokenLimit) =>
            runShareAction(share, () =>
              updateTokenLimitMutation.mutateAsync({
                shareId: share.id,
                tokenLimit,
              }),
            )
          }
          onUpdateParallelLimit={(share, parallelLimit) =>
            runShareAction(share, () =>
              updateParallelLimitMutation.mutateAsync({
                shareId: share.id,
                parallelLimit,
              }),
            )
          }
        />
      </div>

      <CreateShareDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        defaultApp={defaultApp}
        ownerEmail={primaryShare?.ownerEmail ?? null}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        isSubmitting={createMutation.isPending || enableMutation.isPending}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
        onSubmit={handleCreate}
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
          });
        }}
      />
    </div>
  );
}
