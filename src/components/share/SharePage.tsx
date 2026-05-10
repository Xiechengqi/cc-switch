import { useEffect, useMemo, useRef, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  shareApi,
  type AppId,
  type ShareRecord,
  type ShareTunnelStatus,
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
  useUpdateShareAutoStartMutation,
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
import { extractErrorMessage } from "@/utils/errorUtils";
import {
  getTunnelConfigFromSettings,
  isTunnelConfigured,
} from "@/utils/shareUtils";
import { CreateShareDialog } from "./CreateShareDialog";
import { ShareList } from "./ShareList";
import { ShareOwnerChangeEmailDialog } from "./ShareOwnerChangeEmailDialog";
import { ShareOwnerLoginDialog } from "./ShareOwnerLoginDialog";
import { ShareRouterBar } from "./ShareRouterBar";

interface SharePageProps {
  defaultApp?: AppId;
}

export function SharePage({ defaultApp }: SharePageProps) {
  const { t } = useTranslation();
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
  const [ownerLoginEmailHint, setOwnerLoginEmailHint] = useState<string | null>(
    null,
  );
  const [ownerChangeOpen, setOwnerChangeOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ShareRecord | null>(null);
  const [pendingActionShareId, setPendingActionShareId] = useState<
    string | null
  >(null);
  const [pendingOwnerLoginShareId, setPendingOwnerLoginShareId] = useState<
    string | null
  >(null);
  const [
    autoOwnerReverifyPromptedShareId,
    setAutoOwnerReverifyPromptedShareId,
  ] = useState<string | null>(null);
  const ownerLoginRetryAttemptedRef = useRef<Set<string>>(new Set());

  const createMutation = useCreateShareMutation();
  const deleteMutation = useDeleteShareMutation();
  const enableMutation = useEnableShareMutation();
  const disableMutation = useDisableShareMutation();
  const resetUsageMutation = useResetShareUsageMutation();
  const updateApiKeyMutation = useUpdateShareApiKeyMutation();
  const updateDescriptionMutation = useUpdateShareDescriptionMutation();
  const updateForSaleMutation = useUpdateShareForSaleMutation();
  const updateExpirationMutation = useUpdateShareExpirationMutation();
  const updateAutoStartMutation = useUpdateShareAutoStartMutation();
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

  const tunnelOwnerLoginRequiredMap = useMemo(
    () =>
      Object.fromEntries(
        shares.map((share) => [
          share.id,
          tunnelRuntimeStatusMap[share.id]?.requiresOwnerLogin ?? false,
        ]),
      ),
    [shares, tunnelRuntimeStatusMap],
  );

  const primaryShare = shares[0] ?? null;
  const canChangeOwner =
    Boolean(emailAuthStatus?.authenticated) &&
    Boolean(primaryShare?.ownerEmail) &&
    emailAuthStatus?.email === primaryShare?.ownerEmail;
  const shouldShowLoginOnly = !primaryShare && !emailAuthStatus?.authenticated;
  const openOwnerReverify = (share?: ShareRecord | null) => {
    const shareId = share?.id ?? null;
    if (shareId) {
      ownerLoginRetryAttemptedRef.current.delete(shareId);
    }
    setPendingOwnerLoginShareId(shareId);
    setOwnerLoginEmailHint(null);
    setOwnerLoginOpen(true);
  };
  const openOwnerLoginWithEmail = (email?: string | null) => {
    const normalizedEmail = email?.trim().toLowerCase();
    setPendingOwnerLoginShareId(null);
    setOwnerLoginEmailHint(normalizedEmail || null);
    setOwnerLoginOpen(true);
  };

  const isOwnerAuthExpiredError = (error: unknown) => {
    const message = extractErrorMessage(error);
    return (
      message.includes("当前邮箱登录凭证已过期") ||
      message.includes("当前设备身份已失效") ||
      message.includes("请重新发送并验证邮箱验证码") ||
      message.includes("请重新登录") ||
      message.includes("请先完成邮箱验证码登录") ||
      message.includes("当前邮箱登录状态与 share owner 不一致") ||
      message.includes("当前邮箱登录所属分享节点与所选分享节点不一致")
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
    const created = await createMutation.mutateAsync(params);
    setCreateOpen(false);
    await runShareAction(created, () => enableMutation.mutateAsync(created.id));
  };

  const runShareAction = async (
    share: ShareRecord,
    action: () => Promise<unknown>,
    options?: { promptOnOwnerAuthError?: boolean },
  ) => {
    setPendingActionShareId(share.id);
    try {
      await action();
    } catch (error) {
      if (
        options?.promptOnOwnerAuthError !== false &&
        isOwnerAuthExpiredError(error)
      ) {
        openOwnerReverify(share);
      }
      throw error;
    } finally {
      setPendingActionShareId(null);
    }
  };

  const retryPendingShareAfterOwnerLogin = async () => {
    if (!pendingOwnerLoginShareId) return;
    const share = shares.find((item) => item.id === pendingOwnerLoginShareId);
    if (!share) {
      setPendingOwnerLoginShareId(null);
      return;
    }
    setPendingOwnerLoginShareId(null);
    const tunnelInfo = tunnelStatusMap[share.id];
    if (share.status === "active" && tunnelInfo?.healthy) return;
    if (ownerLoginRetryAttemptedRef.current.has(share.id)) return;

    ownerLoginRetryAttemptedRef.current.add(share.id);
    try {
      await runShareAction(
        share,
        () => enableMutation.mutateAsync(share.id),
        { promptOnOwnerAuthError: false },
      );
    } catch {
      // The mutation already shows the detailed failure toast. Do not reopen the
      // login dialog here, otherwise an unchanged router/device state can loop.
    }
  };

  useEffect(() => {
    const share = primaryShare;
    if (!share) return;
    if (!tunnelOwnerLoginRequiredMap[share.id]) return;
    if (autoOwnerReverifyPromptedShareId === share.id) return;
    setAutoOwnerReverifyPromptedShareId(share.id);
    openOwnerReverify(share);
  }, [
    autoOwnerReverifyPromptedShareId,
    primaryShare,
    tunnelOwnerLoginRequiredMap,
  ]);

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
              <Button
                type="button"
                onClick={() => openOwnerLoginWithEmail(emailAuthStatus?.email)}
              >
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
          currentEmail={ownerLoginEmailHint ?? emailAuthStatus?.email ?? null}
          lockedOwnerEmail={null}
          onSaveTunnelConfig={(config) =>
            configureTunnelMutation.mutateAsync(config)
          }
          onVerified={retryPendingShareAfterOwnerLogin}
        />
      </div>
    );
  }

  return (
    <div className="px-6 py-4">
      <div className="mx-auto flex max-w-7xl flex-col gap-5 pb-10">
        <ShareRouterBar
          proxyRunning={proxyStatus?.running ?? false}
          proxyAddress={proxyStatus?.address ?? null}
          proxyPort={proxyStatus?.port ?? null}
          hasShare={shares.length > 0}
          onCreate={() => {
            if (!emailAuthStatus?.authenticated) {
              openOwnerLoginWithEmail(emailAuthStatus?.email);
              return;
            }
            setCreateOpen(true);
          }}
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
          marketsLoading={marketsLoading}
          marketsError={marketsError ? extractErrorMessage(marketsError) : null}
          ownerAuthenticated={canChangeOwner}
          ownerLoginRequiredMap={tunnelOwnerLoginRequiredMap}
          onRetryMarkets={() => void refetchMarkets()}
          onChangeOwner={() => setOwnerChangeOpen(true)}
          onVerifyOwner={() => openOwnerReverify(primaryShare)}
          onRetry={() => void refetch()}
          onCreate={() => {
            if (!emailAuthStatus?.authenticated) {
              openOwnerLoginWithEmail(emailAuthStatus?.email);
              return;
            }
            setCreateOpen(true);
          }}
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
          onUpdateApiKey={(share, apiKey) =>
            runShareAction(share, () =>
              updateApiKeyMutation.mutateAsync({ shareId: share.id, apiKey }),
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
          onUpdateAcl={(share, sharedWithEmails) =>
            runShareAction(share, () =>
              updateAclMutation.mutateAsync({
                shareId: share.id,
                sharedWithEmails,
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
        ownerEmail={emailAuthStatus?.email ?? primaryShare?.ownerEmail ?? null}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        isSubmitting={createMutation.isPending || enableMutation.isPending}
        onRelogin={(email) => {
          setCreateOpen(false);
          openOwnerLoginWithEmail(email);
        }}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
        onSubmit={handleCreate}
      />

      <ShareOwnerLoginDialog
        open={ownerLoginOpen}
        onOpenChange={setOwnerLoginOpen}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        currentEmail={
          ownerLoginEmailHint ??
          emailAuthStatus?.email ??
          primaryShare?.ownerEmail ??
          null
        }
        lockedOwnerEmail={primaryShare?.ownerEmail ?? null}
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
        onVerified={retryPendingShareAfterOwnerLogin}
      />

      <ShareOwnerChangeEmailDialog
        open={ownerChangeOpen}
        onOpenChange={setOwnerChangeOpen}
        tunnelConfig={tunnelConfig}
        tunnelConfigSaving={configureTunnelMutation.isPending}
        currentEmail={
          primaryShare?.ownerEmail ?? emailAuthStatus?.email ?? null
        }
        onSaveTunnelConfig={(config) =>
          configureTunnelMutation.mutateAsync(config)
        }
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
