import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  shareApi,
  type ConnectInfo,
  type CreateShareParams,
  type ShareRecord,
  type TunnelConfig,
} from "@/lib/api";
import type { Settings } from "@/types";
import { extractErrorMessage } from "@/utils/errorUtils";

export const SHARE_REFRESH_INTERVAL_MS = 10000;
const TUNNEL_POLL_INTERVAL_MS = SHARE_REFRESH_INTERVAL_MS;
const SHARE_POLL_INTERVAL_MS = SHARE_REFRESH_INTERVAL_MS;

export const shareKeys = {
  all: ["share"] as const,
  lists: () => [...shareKeys.all, "list"] as const,
  list: () => [...shareKeys.lists()] as const,
  detail: (shareId: string) => [...shareKeys.all, "detail", shareId] as const,
  tunnelStatus: (shareId: string) =>
    [...shareKeys.all, "tunnel-status", shareId] as const,
  connectInfo: (shareId: string) =>
    [...shareKeys.all, "connect-info", shareId] as const,
};

type ShareMutationMessages = {
  successKey: string;
  successDefault: string;
  errorKey: string;
  errorDefault: string;
};

function useShareMutationMessages() {
  const { t } = useTranslation();

  return (
    messages: ShareMutationMessages,
    detail?: string,
  ): { success: string; error: string } => ({
    success: t(messages.successKey, { defaultValue: messages.successDefault }),
    error: t(messages.errorKey, {
      defaultValue: messages.errorDefault,
      error: detail ?? t("common.unknown"),
    }),
  });
}

export function useSharesQuery() {
  return useQuery<ShareRecord[]>({
    queryKey: shareKeys.list(),
    queryFn: shareApi.list,
    refetchInterval: SHARE_POLL_INTERVAL_MS,
    refetchIntervalInBackground: true,
  });
}

export function useShareDetailQuery(shareId?: string | null) {
  return useQuery({
    queryKey: shareId
      ? shareKeys.detail(shareId)
      : [...shareKeys.all, "detail"],
    queryFn: () => shareApi.getDetail(shareId!),
    enabled: Boolean(shareId),
  });
}

export function useShareTunnelStatusQuery(
  shareId?: string | null,
  enabled = false,
  options?: {
    refetchInterval?: number | false;
    refetchIntervalInBackground?: boolean;
  },
) {
  return useQuery({
    queryKey: shareId
      ? shareKeys.tunnelStatus(shareId)
      : [...shareKeys.all, "tunnel-status"],
    queryFn: () => shareApi.getTunnelStatus(shareId!),
    enabled: Boolean(shareId) && enabled,
    refetchInterval: enabled
      ? (options?.refetchInterval ?? TUNNEL_POLL_INTERVAL_MS)
      : false,
    refetchIntervalInBackground: options?.refetchIntervalInBackground ?? true,
  });
}

export function useShareConnectInfoQuery(
  shareId?: string | null,
  enabled = false,
) {
  return useQuery<ConnectInfo>({
    queryKey: shareId
      ? shareKeys.connectInfo(shareId)
      : [...shareKeys.all, "connect-info"],
    queryFn: () => shareApi.getConnectInfo(shareId!),
    enabled: Boolean(shareId) && enabled,
  });
}

function invalidateShareDetail(
  queryClient: ReturnType<typeof useQueryClient>,
  shareId?: string,
) {
  if (!shareId) return Promise.resolve();
  return Promise.all([
    queryClient.invalidateQueries({ queryKey: shareKeys.detail(shareId) }),
    queryClient.invalidateQueries({
      queryKey: shareKeys.tunnelStatus(shareId),
    }),
    queryClient.invalidateQueries({ queryKey: shareKeys.connectInfo(shareId) }),
  ]);
}

export function useCreateShareMutation() {
  const queryClient = useQueryClient();
  const buildMessages = useShareMutationMessages();

  return useMutation({
    mutationFn: (params: CreateShareParams) => shareApi.create(params),
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: shareKeys.list() });
      toast.success(
        buildMessages({
          successKey: "share.toast.createSuccess",
          successDefault: "分享已创建",
          errorKey: "",
          errorDefault: "",
        }).success,
      );
      return created;
    },
    onError: (error: Error) => {
      toast.error(
        buildMessages(
          {
            successKey: "",
            successDefault: "",
            errorKey: "share.toast.createError",
            errorDefault: "创建分享失败: {{error}}",
          },
          extractErrorMessage(error),
        ).error,
      );
    },
  });
}

function useShareActionMutation<TVariables>(
  mutationFn: (variables: TVariables) => Promise<unknown>,
  messages: ShareMutationMessages,
  getShareId: (variables: TVariables) => string | undefined,
) {
  const queryClient = useQueryClient();
  const buildMessages = useShareMutationMessages();

  return useMutation({
    mutationFn,
    onSuccess: async (_data, variables) => {
      const shareId = getShareId(variables);
      await queryClient.invalidateQueries({ queryKey: shareKeys.list() });
      await invalidateShareDetail(queryClient, shareId);
      toast.success(buildMessages(messages).success);
    },
    onError: (error: Error) => {
      toast.error(buildMessages(messages, extractErrorMessage(error)).error);
    },
  });
}

export function useDeleteShareMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.delete(shareId),
    {
      successKey: "share.toast.deleteSuccess",
      successDefault: "分享已删除",
      errorKey: "share.toast.deleteError",
      errorDefault: "删除分享失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function usePauseShareMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.pause(shareId),
    {
      successKey: "share.toast.pauseSuccess",
      successDefault: "分享已暂停",
      errorKey: "share.toast.pauseError",
      errorDefault: "暂停分享失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useResumeShareMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.resume(shareId),
    {
      successKey: "share.toast.resumeSuccess",
      successDefault: "分享已恢复",
      errorKey: "share.toast.resumeError",
      errorDefault: "恢复分享失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useEnableShareMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.enable(shareId),
    {
      successKey: "share.toast.enableSuccess",
      successDefault: "分享已开启",
      errorKey: "share.toast.enableError",
      errorDefault: "开启分享失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useDisableShareMutation() {
  const queryClient = useQueryClient();
  const buildMessages = useShareMutationMessages();

  return useMutation({
    mutationFn: (shareId: string) => shareApi.disable(shareId),
    onSuccess: async (_data, shareId) => {
      queryClient.setQueryData(shareKeys.tunnelStatus(shareId), null);
      queryClient.setQueryData<ShareRecord[] | undefined>(
        shareKeys.list(),
        (current) =>
          current?.map((share) =>
            share.id === shareId
              ? { ...share, status: "paused", tunnelUrl: null }
              : share,
          ),
      );
      queryClient.setQueryData<ShareRecord | null | undefined>(
        shareKeys.detail(shareId),
        (current) =>
          current ? { ...current, status: "paused", tunnelUrl: null } : current,
      );

      await queryClient.invalidateQueries({ queryKey: shareKeys.list() });
      await invalidateShareDetail(queryClient, shareId);
      toast.success(
        buildMessages({
          successKey: "share.toast.disableSuccess",
          successDefault: "分享已关闭",
          errorKey: "share.toast.disableError",
          errorDefault: "关闭分享失败: {{error}}",
        }).success,
      );
    },
    onError: (error: Error) => {
      toast.error(
        buildMessages(
          {
            successKey: "",
            successDefault: "",
            errorKey: "share.toast.disableError",
            errorDefault: "关闭分享失败: {{error}}",
          },
          extractErrorMessage(error),
        ).error,
      );
    },
  });
}

export function useResetShareUsageMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.resetUsage(shareId),
    {
      successKey: "share.toast.resetUsageSuccess",
      successDefault: "用量已重置",
      errorKey: "share.toast.resetUsageError",
      errorDefault: "重置用量失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useUpdateShareTokenLimitMutation() {
  return useShareActionMutation(
    ({ shareId, tokenLimit }: { shareId: string; tokenLimit: number }) =>
      shareApi.updateTokenLimit({ shareId, tokenLimit }),
    {
      successKey: "share.toast.updateTokenLimitSuccess",
      successDefault: "Token 上限已更新",
      errorKey: "share.toast.updateTokenLimitError",
      errorDefault: "更新 Token 上限失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareParallelLimitMutation() {
  return useShareActionMutation(
    ({ shareId, parallelLimit }: { shareId: string; parallelLimit: number }) =>
      shareApi.updateParallelLimit({ shareId, parallelLimit }),
    {
      successKey: "share.toast.updateParallelLimitSuccess",
      successDefault: "最大并发数已更新",
      errorKey: "share.toast.updateParallelLimitError",
      errorDefault: "更新最大并发数失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareSubdomainMutation() {
  return useShareActionMutation(
    ({ shareId, subdomain }: { shareId: string; subdomain: string }) =>
      shareApi.updateSubdomain({ shareId, subdomain }),
    {
      successKey: "share.toast.updateSubdomainSuccess",
      successDefault: "子域名已更新",
      errorKey: "share.toast.updateSubdomainError",
      errorDefault: "更新子域名失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareApiKeyMutation() {
  return useShareActionMutation(
    ({ shareId, apiKey }: { shareId: string; apiKey: string }) =>
      shareApi.updateApiKey({ shareId, apiKey }),
    {
      successKey: "share.toast.updateApiKeySuccess",
      successDefault: "API Key 已更新",
      errorKey: "share.toast.updateApiKeyError",
      errorDefault: "更新 API Key 失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareDescriptionMutation() {
  return useShareActionMutation(
    ({ shareId, description }: { shareId: string; description: string }) =>
      shareApi.updateDescription({ shareId, description }),
    {
      successKey: "share.toast.updateDescriptionSuccess",
      successDefault: "说明已更新",
      errorKey: "share.toast.updateDescriptionError",
      errorDefault: "更新说明失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareForSaleMutation() {
  return useShareActionMutation(
    ({
      shareId,
      forSale,
    }: {
      shareId: string;
      forSale: "Yes" | "No" | "Free";
    }) => shareApi.updateForSale({ shareId, forSale }),
    {
      successKey: "share.toast.updateForSaleSuccess",
      successDefault: "For Sale 已更新",
      errorKey: "share.toast.updateForSaleError",
      errorDefault: "更新 For Sale 失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareExpirationMutation() {
  return useShareActionMutation(
    ({ shareId, expiresAt }: { shareId: string; expiresAt: string }) =>
      shareApi.updateExpiration({ shareId, expiresAt }),
    {
      successKey: "share.toast.updateExpirationSuccess",
      successDefault: "到期时间已更新",
      errorKey: "share.toast.updateExpirationError",
      errorDefault: "更新到期时间失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useUpdateShareAclMutation() {
  return useShareActionMutation(
    ({
      shareId,
      sharedWithEmails,
    }: {
      shareId: string;
      sharedWithEmails: string[];
    }) => shareApi.updateAcl({ shareId, sharedWithEmails }),
    {
      successKey: "share.toast.updateAclSuccess",
      successDefault: "分享名单已更新",
      errorKey: "share.toast.updateAclError",
      errorDefault: "更新分享名单失败: {{error}}",
    },
    ({ shareId }) => shareId,
  );
}

export function useStartShareTunnelMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.startTunnel(shareId),
    {
      successKey: "share.toast.startTunnelSuccess",
      successDefault: "隧道已启动",
      errorKey: "share.toast.startTunnelError",
      errorDefault: "启动隧道失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useStopShareTunnelMutation() {
  return useShareActionMutation(
    (shareId: string) => shareApi.stopTunnel(shareId),
    {
      successKey: "share.toast.stopTunnelSuccess",
      successDefault: "隧道已停止",
      errorKey: "share.toast.stopTunnelError",
      errorDefault: "停止隧道失败: {{error}}",
    },
    (shareId) => shareId,
  );
}

export function useConfigureTunnelMutation() {
  const queryClient = useQueryClient();
  const buildMessages = useShareMutationMessages();

  return useMutation({
    mutationFn: (config: TunnelConfig) => shareApi.configureTunnel(config),
    onSuccess: async (_data, config) => {
      queryClient.setQueryData<Settings | undefined>(
        ["settings"],
        (current) => {
          if (!current) {
            return current;
          }
          return {
            ...current,
            portrDomain: config.domain,
          };
        },
      );
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
      toast.success(
        buildMessages({
          successKey: "share.tunnel.configSaved",
          successDefault: "隧道配置已保存",
          errorKey: "",
          errorDefault: "",
        }).success,
      );
    },
    onError: (error: Error) => {
      toast.error(
        buildMessages(
          {
            successKey: "",
            successDefault: "",
            errorKey: "share.toast.configureTunnelError",
            errorDefault: "保存隧道配置失败: {{error}}",
          },
          extractErrorMessage(error),
        ).error,
      );
    },
  });
}
