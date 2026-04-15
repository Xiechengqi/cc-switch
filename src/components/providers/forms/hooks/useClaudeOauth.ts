import { useState, useCallback, useRef, useEffect } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { authApi } from "@/lib/api";
import type {
  ManagedAuthStatus,
  ManagedAuthDeviceCodeResponse,
} from "@/lib/api";

type AuthState = "idle" | "waiting_browser" | "success" | "error";

export function useClaudeOauth() {
  const queryClient = useQueryClient();
  const queryKey = ["managed-auth-status", "claude_oauth"];

  const [authState, setAuthState] = useState<AuthState>("idle");
  const [deviceCode, setDeviceCode] =
    useState<ManagedAuthDeviceCodeResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const pollingTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const {
    data: authStatus,
    isLoading: isLoadingStatus,
    refetch: refetchStatus,
  } = useQuery<ManagedAuthStatus>({
    queryKey,
    queryFn: () => authApi.authGetStatus("claude_oauth"),
    staleTime: 30000,
  });

  const stopPolling = useCallback(() => {
    if (pollingTimeoutRef.current) {
      clearTimeout(pollingTimeoutRef.current);
      pollingTimeoutRef.current = null;
    }
  }, []);

  useEffect(() => {
    return () => {
      stopPolling();
    };
  }, [stopPolling]);

  const startLoginMutation = useMutation({
    mutationFn: () => authApi.authStartLogin("claude_oauth"),
    onSuccess: async (response) => {
      setDeviceCode(response);
      setAuthState("waiting_browser");
      setError(null);

      // 后端回调任务在后台异步等待，这里用 setTimeout 链式轮询（非 setInterval），
      // 保证每次只有一个轮询请求在飞行中（P1 修复：避免并发 bind 同一端口）
      const interval = (response.interval || 3) * 1000;
      const expiresAt = Date.now() + response.expires_in * 1000;

      const schedulePoll = () => {
        if (Date.now() > expiresAt) {
          stopPolling();
          setAuthState("error");
          setError("授权超时，请重试。");
          return;
        }

        pollingTimeoutRef.current = setTimeout(async () => {
          try {
            const newAccount = await authApi.authPollForAccount(
              "claude_oauth",
              response.device_code,
            );
            if (newAccount) {
              stopPolling();
              setAuthState("success");
              await refetchStatus();
              await queryClient.invalidateQueries({ queryKey });
              setAuthState("idle");
              setDeviceCode(null);
              return;
            }
          } catch (e) {
            const errorMessage = e instanceof Error ? e.message : String(e);
            if (
              !errorMessage.includes("pending") &&
              !errorMessage.includes("slow_down") &&
              !errorMessage.includes("authorization_pending")
            ) {
              stopPolling();
              setAuthState("error");
              setError(errorMessage);
              return;
            }
          }
          // 本次轮询完成后再安排下一次
          schedulePoll();
        }, interval);
      };

      schedulePoll();
    },
    onError: (e) => {
      setAuthState("error");
      setError(e instanceof Error ? e.message : String(e));
    },
  });

  const logoutMutation = useMutation({
    mutationFn: () => authApi.authLogout("claude_oauth"),
    onSuccess: async () => {
      setAuthState("idle");
      setDeviceCode(null);
      setError(null);
      queryClient.setQueryData(queryKey, {
        provider: "claude_oauth",
        authenticated: false,
        default_account_id: null,
        accounts: [],
      });
      await queryClient.invalidateQueries({ queryKey });
    },
    onError: async (e) => {
      console.error("[ClaudeOAuth] Failed to logout:", e);
      setError(e instanceof Error ? e.message : String(e));
      await refetchStatus();
    },
  });

  const removeAccountMutation = useMutation({
    mutationFn: (accountId: string) =>
      authApi.authRemoveAccount("claude_oauth", accountId),
    onSuccess: async () => {
      setAuthState("idle");
      setDeviceCode(null);
      setError(null);
      await refetchStatus();
      await queryClient.invalidateQueries({ queryKey });
    },
    onError: (e) => {
      console.error("[ClaudeOAuth] Failed to remove account:", e);
      setError(e instanceof Error ? e.message : String(e));
    },
  });

  const setDefaultAccountMutation = useMutation({
    mutationFn: (accountId: string) =>
      authApi.authSetDefaultAccount("claude_oauth", accountId),
    onSuccess: async () => {
      await refetchStatus();
      await queryClient.invalidateQueries({ queryKey });
    },
    onError: (e) => {
      console.error("[ClaudeOAuth] Failed to set default account:", e);
      setError(e instanceof Error ? e.message : String(e));
    },
  });

  const startAuth = useCallback(() => {
    setAuthState("idle");
    setDeviceCode(null);
    setError(null);
    stopPolling();
    startLoginMutation.mutate();
  }, [startLoginMutation, stopPolling]);

  const cancelAuth = useCallback(() => {
    stopPolling();
    setAuthState("idle");
    setDeviceCode(null);
    setError(null);
  }, [stopPolling]);

  const logout = useCallback(() => {
    logoutMutation.mutate();
  }, [logoutMutation]);

  const removeAccount = useCallback(
    (accountId: string) => {
      removeAccountMutation.mutate(accountId);
    },
    [removeAccountMutation],
  );

  const setDefaultAccount = useCallback(
    (accountId: string) => {
      setDefaultAccountMutation.mutate(accountId);
    },
    [setDefaultAccountMutation],
  );

  const accounts = authStatus?.accounts ?? [];

  return {
    authStatus,
    isLoadingStatus,
    accounts,
    hasAnyAccount: accounts.length > 0,
    isAuthenticated: authStatus?.authenticated ?? false,
    defaultAccountId: authStatus?.default_account_id ?? null,
    authState,
    deviceCode,
    error,
    isWaitingBrowser: authState === "waiting_browser",
    isAddingAccount:
      startLoginMutation.isPending || authState === "waiting_browser",
    isRemovingAccount: removeAccountMutation.isPending,
    isSettingDefaultAccount: setDefaultAccountMutation.isPending,
    startAuth,
    addAccount: startAuth,
    cancelAuth,
    logout,
    removeAccount,
    setDefaultAccount,
    refetchStatus,
  };
}
