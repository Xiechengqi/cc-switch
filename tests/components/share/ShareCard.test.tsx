import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createElement, type ReactNode } from "react";
import { describe, it, expect, vi } from "vitest";
import { ShareCard } from "@/components/share/ShareCard";
import type { ShareRecord } from "@/lib/api";
import { createTestQueryClient } from "../../utils/testQueryClient";

vi.mock("@/components/share/ShareRequestLogTable", () => ({
  ShareRequestLogTable: () => null,
}));

const tunnelConfig = {
  domain: "127.0.0.1:8787",
} as const;

const baseShare: ShareRecord = {
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
  tokensUsed: 500,
  requestsCount: 3,
  expiresAt: "2026-05-01T00:00:00.000Z",
  subdomain: null,
  tunnelUrl: null,
  status: "active",
  createdAt: "2026-04-01T00:00:00.000Z",
  lastUsedAt: null,
};

describe("ShareCard", () => {
  const renderShareCard = (ui: ReactNode) => {
    const client = createTestQueryClient();
    return render(createElement(QueryClientProvider, { client }, ui));
  };

  it("shows disable for active share even when tunnel is not configured", () => {
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={false}
        onOpenDetail={vi.fn()}
        onOpenConnect={vi.fn()}
        onDelete={vi.fn()}
        onEnable={vi.fn()}
        onDisable={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "share.disable" })).toBeEnabled();
  });

  it("shows disable when share is active but tunnel is offline", () => {
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        onOpenDetail={vi.fn()}
        onOpenConnect={vi.fn()}
        onDelete={vi.fn()}
        onEnable={vi.fn()}
        onDisable={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "share.disable" })).toBeEnabled();
  });

  it("calls disable handler for active share", async () => {
    const user = userEvent.setup();
    const onDisable = vi.fn();
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        onOpenDetail={vi.fn()}
        onOpenConnect={vi.fn()}
        onDelete={vi.fn()}
        onEnable={vi.fn()}
        onDisable={onDisable}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.disable" }));
    expect(onDisable).toHaveBeenCalledWith(baseShare);
  });

  it("shows enable for paused share even if a stale tunnel url exists", () => {
    renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          status: "paused",
          tunnelUrl: "http://share-1.127.0.0.1:8787",
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        onOpenDetail={vi.fn()}
        onOpenConnect={vi.fn()}
        onDelete={vi.fn()}
        onEnable={vi.fn()}
        onDisable={vi.fn()}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "share.disable" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "share.enable" })).toBeEnabled();
  });
});
