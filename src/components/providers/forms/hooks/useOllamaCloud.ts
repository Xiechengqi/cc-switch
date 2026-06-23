import { useCallback, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { authApi } from "@/lib/api";
import type { OllamaCloudStatus, OllamaModel } from "@/lib/api";

type ImportApiKeyInput = {
  apiKey: string;
  label?: string | null;
};

export function useOllamaCloud() {
  const queryClient = useQueryClient();
  const queryKey = ["ollama-cloud-status"];
  const [error, setError] = useState<string | null>(null);

  const {
    data: authStatus,
    isLoading: isLoadingStatus,
    refetch: refetchStatus,
  } = useQuery<OllamaCloudStatus>({
    queryKey,
    queryFn: () => authApi.ollamaCloudStatus(),
    staleTime: 30000,
  });

  const invalidate = useCallback(async () => {
    setError(null);
    await refetchStatus();
    await queryClient.invalidateQueries({ queryKey });
  }, [queryClient, refetchStatus]);

  const importApiKeyMutation = useMutation({
    mutationFn: (input: ImportApiKeyInput) =>
      authApi.ollamaCloudImportApiKey(input),
    onSuccess: invalidate,
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });

  const removeAccountMutation = useMutation({
    mutationFn: (accountId: string) =>
      authApi.ollamaCloudRemoveAccount(accountId),
    onSuccess: invalidate,
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });

  const setDefaultAccountMutation = useMutation({
    mutationFn: (accountId: string) =>
      authApi.ollamaCloudSetDefaultAccount(accountId),
    onSuccess: invalidate,
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });

  const testConnectionMutation = useMutation({
    mutationFn: (apiKey: string) => authApi.ollamaCloudTestConnection(apiKey),
    onSuccess: () => setError(null),
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });

  const listModelsMutation = useMutation({
    mutationFn: (accountId?: string | null) =>
      authApi.ollamaCloudListModels(accountId),
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });

  const accounts = authStatus?.accounts ?? [];

  return {
    authStatus,
    isLoadingStatus,
    accounts,
    hasAnyAccount: accounts.length > 0,
    isAuthenticated: authStatus?.authenticated ?? false,
    defaultAccountId: authStatus?.defaultAccountId ?? null,
    error,
    isImportingApiKey: importApiKeyMutation.isPending,
    isRemovingAccount: removeAccountMutation.isPending,
    isSettingDefaultAccount: setDefaultAccountMutation.isPending,
    isTestingConnection: testConnectionMutation.isPending,
    isListingModels: listModelsMutation.isPending,
    importApiKey: (input: ImportApiKeyInput) =>
      importApiKeyMutation.mutateAsync(input),
    removeAccount: (accountId: string) => removeAccountMutation.mutate(accountId),
    setDefaultAccount: (accountId: string) =>
      setDefaultAccountMutation.mutate(accountId),
    testConnection: (apiKey: string): Promise<OllamaModel[]> =>
      testConnectionMutation.mutateAsync(apiKey),
    listModels: (accountId?: string | null): Promise<OllamaModel[]> =>
      listModelsMutation.mutateAsync(accountId),
    refetchStatus,
  };
}
