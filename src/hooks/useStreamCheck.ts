import { useState, useCallback } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  streamCheckProvider,
  type StreamCheckResult,
} from "@/lib/api/model-test";
import { useResetCircuitBreaker } from "@/lib/query/failover";
import type { AppId } from "@/lib/api";

/**
 * 供应商连通性检查。
 *
 * 只探测 base_url 是否可达（任何 HTTP 响应都算可达），不发真实大模型请求。
 * 刻意 **不** 重置故障转移熔断器——可达 ≠ 配置正确，一个端口通但鉴权废的供应商
 * 不应被误判为"健康"而切回线上。熔断器只由真实转发流量驱动（见 proxy/forwarder.rs）。
 */
export function useStreamCheck(appId: AppId) {
  const { t } = useTranslation();
  const [checkingIds, setCheckingIds] = useState<Set<string>>(new Set());
  const resetCircuitBreaker = useResetCircuitBreaker();

  const checkProvider = useCallback(
    async (
      providerId: string,
      providerName: string,
    ): Promise<StreamCheckResult | null> => {
      setCheckingIds((prev) => new Set(prev).add(providerId));

      try {
        const result = await streamCheckProvider(appId, providerId);

        if (result.status === "operational") {
          toast.success(
            t("streamCheck.reachable", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue: `${providerName} 连通正常 (${result.responseTimeMs}ms)`,
            }),
            { closeButton: true },
          );
        } else if (result.status === "degraded") {
          toast.warning(
            t("streamCheck.reachableSlow", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue: `${providerName} 连通但较慢 (${result.responseTimeMs}ms)`,
            }),
          );

          // 降级状态也重置熔断器，因为至少能通信
          resetCircuitBreaker.mutate({ providerId, appType: appId });
        } else if (result.errorCategory === "modelNotFound") {
          // 专门处理"模型不存在/已下架"：指向配置入口，比通用 404 文案更有指导性
          toast.error(
            t("streamCheck.modelNotFound", {
              providerName: providerName,
              model: result.modelUsed,
              defaultValue: `${providerName} 测试模型 ${result.modelUsed} 不存在或已下架`,
            }),
            {
              description: t("streamCheck.modelNotFoundHint", {
                defaultValue: "",
              }),
              duration: 10000,
              closeButton: true,
            },
          );
        } else if (result.errorCategory === "quotaExceeded") {
          toast.warning(
            t("streamCheck.quotaExceeded", {
              providerName: providerName,
              defaultValue: `${providerName} Coding Plan quota has been exceeded`,
            }),
            {
              description: t("streamCheck.quotaExceededHint", {
                defaultValue: "",
              }),
              duration: 10000,
              closeButton: true,
            },
          );
        } else if (result.errorCategory === "codexOauthTokenInvalidated") {
          toast.warning(
            t("streamCheck.codexOauthTokenInvalidated", {
              providerName: providerName,
              defaultValue: `${providerName} OAuth token has been invalidated`,
            }),
            {
              description: t("streamCheck.codexOauthTokenInvalidatedHint", {
                defaultValue:
                  "cc-switch retried after refreshing the token, but OpenAI still rejected it. Sign in with OpenAI Official (OAuth) again.",
              }),
              duration: 10000,
              closeButton: true,
            },
          );
        } else if (result.errorCategory === "openaiSessionTokenInvalidated") {
          toast.warning(
            t("streamCheck.openaiSessionTokenInvalidated", {
              providerName: providerName,
              defaultValue: `${providerName} session token has been invalidated`,
            }),
            {
              description: t("streamCheck.openaiSessionTokenInvalidatedHint", {
                defaultValue:
                  "Sign in to chatgpt.com again, fetch /api/auth/session, and re-import the JSON.",
              }),
              duration: 10000,
              closeButton: true,
            },
          );
        } else if (result.errorCategory === "tokenInvalidated") {
          toast.warning(
            t("streamCheck.tokenInvalidated", {
              providerName: providerName,
              defaultValue: `${providerName} authentication token has been invalidated`,
            }),
            {
              description: t("streamCheck.tokenInvalidatedHint", {
                defaultValue: "Refresh the managed account sign-in and try again.",
              }),
              duration: 10000,
              closeButton: true,
            },
          );
        } else {
          // 仅当无法建立连接（DNS / 连接被拒 / TLS / 超时）才会到这里
          toast.error(
            t("streamCheck.unreachable", {
              providerName: providerName,
              message: result.message,
              defaultValue: `${providerName} 无法连通: ${result.message}`,
            }),
            {
              description: t("streamCheck.unreachableHint", {
                defaultValue:
                  "无法建立连接（DNS / 连接 / TLS / 超时）。请检查 base_url 与网络。",
              }),
              duration: 8000,
              closeButton: true,
            },
          );
        }

        return result;
      } catch (e) {
        toast.error(
          t("streamCheck.error", {
            providerName: providerName,
            error: String(e),
            defaultValue: `${providerName} 检查出错: ${String(e)}`,
          }),
        );
        return null;
      } finally {
        setCheckingIds((prev) => {
          const next = new Set(prev);
          next.delete(providerId);
          return next;
        });
      }
    },
    [appId, t],
  );

  const isChecking = useCallback(
    (providerId: string) => checkingIds.has(providerId),
    [checkingIds],
  );

  return { checkProvider, isChecking };
}
