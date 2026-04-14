import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { ShareCard } from "@/components/share/ShareCard";

const tunnelConfig = {
  domain: "127.0.0.1:8787",
} as const;

const baseShare = {
  id: "share-1",
  name: "Demo Share",
  shareToken: "token",
  appType: "proxy",
  providerId: null,
  apiKey: "",
  settingsConfig: null,
  tokenLimit: 1000,
  tokensUsed: 500,
  requestsCount: 3,
  expiresAt: "2026-05-01T00:00:00.000Z",
  subdomain: null,
  tunnelUrl: null,
  status: "active",
  createdAt: "2026-04-01T00:00:00.000Z",
  lastUsedAt: null,
} as const;

describe("ShareCard", () => {
  it("disables start tunnel when tunnel is not configured", () => {
    render(
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

    expect(screen.getByRole("button", { name: "share.enable" })).toBeDisabled();
  });

  it("allows enable when share is active but tunnel is offline", () => {
    render(
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

    expect(screen.getByRole("button", { name: "share.enable" })).toBeEnabled();
  });

  it("calls disable handler for active share", async () => {
    const user = userEvent.setup();
    const onDisable = vi.fn();
    render(
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
});
