import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { createTestQueryClient } from "../../utils/testQueryClient";
import {
  useConfigureTunnelMutation,
  useCreateShareMutation,
  shareKeys,
} from "@/lib/query";

const toastSuccess = vi.fn();
const toastError = vi.fn();

const createMock = vi.fn();
const configureMock = vi.fn();

vi.mock("sonner", () => ({
  toast: {
    success: (...args: unknown[]) => toastSuccess(...args),
    error: (...args: unknown[]) => toastError(...args),
  },
}));

vi.mock("@/lib/api", async () => {
  const actual = await vi.importActual<typeof import("@/lib/api")>("@/lib/api");
  return {
    ...actual,
    shareApi: {
      create: (...args: unknown[]) => createMock(...args),
      configureTunnel: (...args: unknown[]) => configureMock(...args),
      delete: vi.fn(),
      pause: vi.fn(),
      resume: vi.fn(),
      list: vi.fn(),
      getDetail: vi.fn(),
      startTunnel: vi.fn(),
      stopTunnel: vi.fn(),
      getTunnelStatus: vi.fn(),
      getConnectInfo: vi.fn(),
    },
  };
});

describe("share query hooks", () => {
  beforeEach(() => {
    toastSuccess.mockReset();
    toastError.mockReset();
    createMock.mockReset();
    configureMock.mockReset();
  });

  it("invalidates share list after create", async () => {
    createMock.mockResolvedValue({
      id: "share-1",
      name: "Demo Share",
      ownerEmail: "owner@example.com",
      sharedWithEmails: [],
      forSale: "No",
      shareToken: "token",
      appType: "proxy",
      providerId: null,
      apiKey: "",
      settingsConfig: null,
      tokenLimit: 1000,
      parallelLimit: 3,
      tokensUsed: 0,
      requestsCount: 0,
      expiresAt: new Date().toISOString(),
      subdomain: null,
      tunnelUrl: null,
      status: "paused",
      createdAt: new Date().toISOString(),
      lastUsedAt: null,
    });
    const client = createTestQueryClient();
    const invalidateSpy = vi.spyOn(client, "invalidateQueries");
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);

    const { result } = renderHook(() => useCreateShareMutation(), { wrapper });
    await result.current.mutateAsync({
      appType: "claude",
      forSale: "No",
      tokenLimit: 1000,
      parallelLimit: 3,
      expiresInSecs: 3600,
    });

    await waitFor(() =>
      expect(invalidateSpy).toHaveBeenCalledWith({
        queryKey: shareKeys.list(),
      }),
    );
  });

  it("invalidates settings after tunnel config save", async () => {
    configureMock.mockResolvedValue(undefined);
    const client = createTestQueryClient();
    const invalidateSpy = vi.spyOn(client, "invalidateQueries");
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);

    const { result } = renderHook(() => useConfigureTunnelMutation(), {
      wrapper,
    });

    await result.current.mutateAsync({
      domain: "server.example.com",
    });

    await waitFor(() =>
      expect(invalidateSpy).toHaveBeenCalledWith({
        queryKey: ["settings"],
      }),
    );
  });
});
