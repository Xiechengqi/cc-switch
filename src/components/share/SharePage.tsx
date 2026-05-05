import { useEffect, useMemo, useState } from "react";
import { useQueries, useQueryClient, type Query } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  settingsApi,
  shareApi,
  type AppId,
  type ShareRecord,
  type TunnelInfo,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { useSettingsQuery } from "@/lib/query";
import { useProxyStatus } from "@/lib/query/proxy";
import {
  useConfigureTunnelMutation,
  useCreateShareMutation,
  useDeleteShareMutation,
  useEmailAuthStatusQuery,
  useShareMarketsQuery,
  useDisableShareMutation,
  useEnableShareMutation,
  useResetShareUsageMutation,
  useUpdateShareAclMutation,
  useSharesQuery,
  useUpdateShareApiKeyMutation,
  useUpdateShareDescriptionMutation,
  useUpdateShareExpirationMutation,
  useUpdateShareForSaleMutation,
  useUpdateShareParallelLimitMutation,
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
import { ShareOwnerChangeEmailDialog } from "./ShareOwnerChangeEmailDialog";
import { ShareOwnerLoginDialog } from "./ShareOwnerLoginDialog";
import { ShareRouterBar } from "./ShareRouterBar";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

interface SharePageProps {
  defaultApp?: AppId;
}

export function SharePage({ defaultApp }: SharePageProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: shares = [], isLoading, error, refetch } = useSharesQuery();
  const { data: settings } = useSettingsQuery();
  const { data: emailAuthStatus } = useEmailAuthStatusQuery();
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
  const [ownerLoginOpen, setOwnerLoginOpen] = useState(false);
  const [ownerChangeOpen, setOwnerChangeOpen] = useState(false);
  const [detailShareId, setDetailShareId] = useState<string | null>(null);
  const [connectShareId, setConnectShareId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ShareRecord | null>(null);
  const [pendingActionShareId, setPendingActionShareId] = useState<
    string | null
  >(null);
  const [pendingOwnerLoginShareId, setPendingOwnerLoginShareId] = useState<
    string | null
  >(null);
  const [selectedMarketEmail, setSelectedMarketEmail] = useState("");
  const [marketActionPending, setMarketActionPending] = useState(false);

  const createMutation = useCreateShareMutation();
  const deleteMutation = useDeleteShareMutation();
  const enableMutation = useEnableShareMutation();
  const disableMutation = useDisableShareMutation();
  const resetUsageMutation = useResetShareUsageMutation();
  const updateApiKeyMutation = useUpdateShareApiKeyMutation();
  const updateDescriptionMutation = useUpdateShareDescriptionMutation();
  const updateForSaleMutation = useUpdateShareForSaleMutation();
  const updateExpirationMutation = useUpdateShareExpirationMutation();
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
  const selectedMarket =
    markets.find((market) => market.email === selectedMarketEmail) ?? null;
  const authorizedMarket = primaryShare
    ? markets.find((market) =>
        primaryShare.sharedWithEmails
          .map((email) => email.toLowerCase())
          .includes(market.email.toLowerCase()),
      ) ?? null
    : null;
  const canChangeOwner =
    Boolean(emailAuthStatus?.authenticated) &&
    Boolean(primaryShare?.ownerEmail) &&
    emailAuthStatus?.email === primaryShare?.ownerEmail;
  const shouldShowLoginOnly = !primaryShare && !emailAuthStatus?.authenticated;
  const openOwnerReverify = (share?: ShareRecord | null) => {
    setPendingOwnerLoginShareId(share?.id ?? null);
    setOwnerLoginOpen(true);
  };

  const isOwnerAuthExpiredError = (error: unknown) => {
    const message = extractErrorMessage(error);
    return (
      message.includes("当前邮箱登录凭证已过期") ||
      message.includes("请重新登录") ||
      message.includes("请先完成邮箱验证码登录")
    );
  };

  const handleCreate = async (
    params: Parameters<typeof createMutation.mutateAsync>[0],
  ) => {
    if (!emailAuthStatus?.authenticated || !emailAuthStatus.email) {
      setOwnerLoginOpen(true);
      throw new Error(
        t("share.emailLoginRequired", {
          defaultValue: "请先完成 share owner 邮箱登录，再创建 share",
        }),
      );
    }
    await createMutation.mutateAsync(params);
    setCreateOpen(false);
  };

  const handleAuthorizeMarket = async () => {
    if (!primaryShare || !selectedMarket) return;
    setMarketActionPending(true);
    try {
      const updated = await shareApi.authorizeMarket(
        primaryShare.id,
        selectedMarket.email,
      );
      queryClient.setQueryData<ShareRecord[] | undefined>(
        shareKeys.list(),
        (current) =>
          current?.map((share) => (share.id === updated.id ? updated : share)),
      );
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: shareKeys.list() }),
        queryClient.invalidateQueries({
          queryKey: shareKeys.detail(primaryShare.id),
        }),
      ]);
      toast.success(
        t("share.market.authorizeSuccess", {
          defaultValue: "Market authorized",
        }),
      );
    } catch (error) {
      if (isOwnerAuthExpiredError(error)) {
        openOwnerReverify(primaryShare);
      }
      toast.error(
        t("share.market.authorizeError", {
          defaultValue: "Authorize market failed: {{error}}",
          error: extractErrorMessage(error),
        }),
      );
    } finally {
      setMarketActionPending(false);
    }
  };

  const runShareAction = async (
    share: ShareRecord,
    action: () => Promise<unknown>,
  ) => {
    setPendingActionShareId(share.id);
    try {
      await action();
    } catch (error) {
      if (isOwnerAuthExpiredError(error)) {
        openOwnerReverify(share);
      }
      throw error;
    } finally {
      setPendingActionShareId(null);
    }
  };

  useEffect(() => {
    if (!pendingOwnerLoginShareId || !emailAuthStatus?.authenticated) return;
    const share = shares.find((item) => item.id === pendingOwnerLoginShareId);
    if (!share || emailAuthStatus.email !== share.ownerEmail) return;
    setPendingOwnerLoginShareId(null);
    if (share.status !== "active") {
      void runShareAction(share, () => enableMutation.mutateAsync(share.id));
    }
  }, [emailAuthStatus, enableMutation, pendingOwnerLoginShareId, shares]);

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

  if (shouldShowLoginOnly) {
    return (
      <div className="px-6 py-4">
        <div className="mx-auto flex max-w-3xl flex-col gap-4 pb-10">
          <div className="rounded-xl border border-border-default/70 bg-card/80 p-6">
            <div className="space-y-2">
              <h3 className="text-lg font-semibold">
                {t("share.ownerLogin.emptyTitle", {
                  defaultValue: "Login to create a share",
                })}
              </h3>
              <p className="text-sm text-muted-foreground">
                {t("share.ownerLogin.emptyDescription", {
                  defaultValue:
                    "Choose a share router and verify the owner email before creating a share.",
                })}
              </p>
            </div>
            <div className="mt-5 flex flex-wrap gap-2">
              <Button type="button" onClick={() => setOwnerLoginOpen(true)}>
                {t("share.ownerLogin.action", {
                  defaultValue: "Login Share Owner",
                })}
              </Button>
            </div>
          </div>
        </div>
        <ShareOwnerLoginDialog
          open={ownerLoginOpen}
          onOpenChange={setOwnerLoginOpen}
          tunnelConfig={tunnelConfig}
          tunnelConfigSaving={configureTunnelMutation.isPending}
          currentEmail={emailAuthStatus?.email ?? null}
          lockedOwnerEmail={null}
          onSaveTunnelConfig={(config) =>
            configureTunnelMutation.mutateAsync(config)
          }
        />
      </div>
    );
  }

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
          ownerEmail={primaryShare?.ownerEmail ?? null}
          ownerAuthenticated={canChangeOwner}
          onCreate={() => {
            if (!emailAuthStatus?.authenticated) {
              setOwnerLoginOpen(true);
              return;
            }
            setCreateOpen(true);
          }}
          onChangeOwner={() => setOwnerChangeOpen(true)}
          onVerifyOwner={() => openOwnerReverify(primaryShare)}
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
          onCreate={() => {
            if (!emailAuthStatus?.authenticated) {
              setOwnerLoginOpen(true);
              return;
            }
            setCreateOpen(true);
          }}
          onOpenDetail={(share) => setDetailShareId(share.id)}
          onOpenConnect={(share) => setConnectShareId(share.id)}
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
          onRefresh={(share) => void handleRefresh(share)}
        />

        {primaryShare && (
          <div className="rounded-lg border border-border-default bg-card p-4">
            <div className="flex flex-col gap-3 md:flex-row md:items-end md:justify-between">
              <div className="min-w-0 space-y-1">
                <div className="text-sm font-semibold">
                  {t("share.market.title", {
                    defaultValue: "Market",
                  })}
                </div>
                <div className="text-xs text-muted-foreground">
                  {authorizedMarket
                    ? t("share.market.authorized", {
                        defaultValue: "Authorized: {{market}}",
                        market: authorizedMarket.displayName,
                      })
                    : t("share.market.description", {
                        defaultValue:
                          "Choose a market to sell this share and claim earnings there.",
                      })}
                </div>
              </div>
              <div className="flex w-full flex-col gap-2 md:w-auto md:min-w-[360px] md:flex-row">
                <Select
                  value={selectedMarketEmail}
                  onValueChange={setSelectedMarketEmail}
                  disabled={marketsLoading || markets.length === 0}
                >
                  <SelectTrigger className="md:w-[220px]">
                    <SelectValue
                      placeholder={
                        marketsLoading
                          ? t("common.loading", { defaultValue: "Loading" })
                          : t("share.market.select", {
                              defaultValue: "Select market",
                            })
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {markets.map((market) => (
                      <SelectItem key={market.id} value={market.email}>
                        {market.displayName}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Button
                  type="button"
                  onClick={() => void handleAuthorizeMarket()}
                  disabled={
                    !selectedMarket ||
                    marketActionPending ||
                    primaryShare.sharedWithEmails
                      .map((email) => email.toLowerCase())
                      .includes(selectedMarket.email.toLowerCase())
                  }
                >
                  {t("share.market.authorize", {
                    defaultValue: "Authorize",
                  })}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={!authorizedMarket}
                  onClick={() => {
                    if (!authorizedMarket) return;
                    void settingsApi.openExternal(
                      `${authorizedMarket.publicBaseUrl.replace(/\/$/, "")}/claim`,
                    );
                  }}
                >
                  {t("share.market.claim", {
                    defaultValue: "Claim earnings",
                  })}
                </Button>
              </div>
            </div>
            {marketsError && (
              <button
                type="button"
                className="mt-2 text-xs text-destructive underline"
                onClick={() => void refetchMarkets()}
              >
                {extractErrorMessage(marketsError)}
              </button>
            )}
          </div>
        )}
      </div>

      <CreateShareDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        defaultApp={defaultApp}
        ownerEmail={emailAuthStatus?.email ?? primaryShare?.ownerEmail ?? null}
        isSubmitting={createMutation.isPending}
        onSubmit={handleCreate}
      />

      <ShareOwnerLoginDialog
        open={ownerLoginOpen}
        onOpenChange={setOwnerLoginOpen}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        currentEmail={
          emailAuthStatus?.email ?? primaryShare?.ownerEmail ?? null
        }
        lockedOwnerEmail={primaryShare?.ownerEmail ?? null}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
      />

      <ShareOwnerChangeEmailDialog
        open={ownerChangeOpen}
        onOpenChange={setOwnerChangeOpen}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        currentEmail={primaryShare?.ownerEmail ?? emailAuthStatus?.email ?? null}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
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
        onUpdateAcl={(share, sharedWithEmails) =>
          void runShareAction(share, () =>
            updateAclMutation.mutateAsync({
              shareId: share.id,
              sharedWithEmails,
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
        onUpdateParallelLimit={(share, parallelLimit) =>
          void runShareAction(share, () =>
            updateParallelLimitMutation.mutateAsync({
              shareId: share.id,
              parallelLimit,
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
