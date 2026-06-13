import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { openaiSessionApi } from "@/lib/api";

const queryKey = ["openai-session-status"];

export function useOpenAISession() {
  const queryClient = useQueryClient();
  const {
    data: status,
    isLoading: isLoadingStatus,
    refetch: refetchStatus,
  } = useQuery({
    queryKey,
    queryFn: openaiSessionApi.getOpenAISessionStatus,
    staleTime: 30000,
  });

  const importMutation = useMutation({
    mutationFn: openaiSessionApi.importOpenAISession,
    onSuccess: async () => {
      await refetchStatus();
      await queryClient.invalidateQueries({ queryKey });
    },
  });

  const removeMutation = useMutation({
    mutationFn: openaiSessionApi.removeOpenAISession,
    onSuccess: async () => {
      await refetchStatus();
      await queryClient.invalidateQueries({ queryKey });
    },
  });

  const setDefaultMutation = useMutation({
    mutationFn: openaiSessionApi.setDefaultOpenAISession,
    onSuccess: async () => {
      await refetchStatus();
      await queryClient.invalidateQueries({ queryKey });
    },
  });

  return {
    status,
    isLoadingStatus,
    accounts: status?.accounts ?? [],
    hasAnyAccount: (status?.accounts ?? []).length > 0,
    isAuthenticated: status?.authenticated ?? false,
    defaultAccountId: status?.default_account_id ?? null,
    importSession: importMutation.mutateAsync,
    removeAccount: removeMutation.mutate,
    setDefaultAccount: setDefaultMutation.mutate,
    isImporting: importMutation.isPending,
    isRemovingAccount: removeMutation.isPending,
    isSettingDefaultAccount: setDefaultMutation.isPending,
    refetchStatus,
  };
}
