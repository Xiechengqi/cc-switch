import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ShareDetailDrawer } from "@/components/share/ShareDetailDrawer";
import type { ShareRecord } from "@/lib/api";

const baseShare: ShareRecord = {
  id: "share-1",
  name: "Demo Share",
  ownerEmail: "owner@example.com",
  sharedWithEmails: [],
  description: null,
  forSale: "No",
  shareToken: "token-demo",
  appType: "proxy",
  providerId: null,
  apiKey: "",
  settingsConfig: null,
  tokenLimit: 1000,
  parallelLimit: 3,
  tokensUsed: 500,
  requestsCount: 3,
  expiresAt: "2026-05-01T00:00:00.000Z",
  subdomain: "demo",
  tunnelUrl: "https://demo.example.com",
  status: "active",
  createdAt: "2026-04-01T00:00:00.000Z",
  lastUsedAt: null,
};

describe("ShareDetailDrawer", () => {
  it("shows -1 in a disabled token limit input for unlimited shares", () => {
    render(
      <ShareDetailDrawer
        share={{ ...baseShare, tokenLimit: -1, tokensUsed: 1_200_000 }}
        tunnelStatus={null}
        tunnelConfig={{ domain: "server.example.com" }}
        open={true}
        onOpenChange={vi.fn()}
        onResetUsage={vi.fn()}
        onUpdateTokenLimit={vi.fn()}
        onUpdateParallelLimit={vi.fn()}
        onUpdateSubdomain={vi.fn()}
        onUpdateApiKey={vi.fn()}
        onUpdateDescription={vi.fn()}
        onUpdateForSale={vi.fn()}
        onUpdateExpiration={vi.fn()}
        onUpdateAcl={vi.fn()}
      />,
    );

    const tokenLimitInputs = screen.getAllByDisplayValue("-1");
    const numericInput = tokenLimitInputs.find(
      (element) => element.getAttribute("type") === "number",
    );

    expect(numericInput).toBeDefined();
    expect(screen.getAllByLabelText("share.unlimited")[0]).toBeChecked();
    expect(numericInput).toBeDisabled();
    expect(screen.getByText("1.2M/∞")).toBeInTheDocument();
  });

  it("restores a finite token limit when unlimited is unchecked", async () => {
    const user = userEvent.setup();

    render(
      <ShareDetailDrawer
        share={baseShare}
        tunnelStatus={null}
        tunnelConfig={{ domain: "server.example.com" }}
        open={true}
        onOpenChange={vi.fn()}
        onResetUsage={vi.fn()}
        onUpdateTokenLimit={vi.fn()}
        onUpdateParallelLimit={vi.fn()}
        onUpdateSubdomain={vi.fn()}
        onUpdateApiKey={vi.fn()}
        onUpdateDescription={vi.fn()}
        onUpdateForSale={vi.fn()}
        onUpdateExpiration={vi.fn()}
        onUpdateAcl={vi.fn()}
      />,
    );

    const toggle = screen.getAllByLabelText("share.unlimited")[0];
    const tokenLimitInput = screen.getByDisplayValue("1000");

    await user.click(toggle);
    expect(tokenLimitInput).toHaveValue(-1);
    expect(tokenLimitInput).toBeDisabled();

    await user.click(toggle);
    expect(tokenLimitInput).toHaveValue(1000);
    expect(tokenLimitInput).toBeEnabled();
  });

  it("restores a finite parallel limit when unlimited is unchecked", async () => {
    const user = userEvent.setup();

    render(
      <ShareDetailDrawer
        share={baseShare}
        tunnelStatus={null}
        tunnelConfig={{ domain: "server.example.com" }}
        open={true}
        onOpenChange={vi.fn()}
        onResetUsage={vi.fn()}
        onUpdateTokenLimit={vi.fn()}
        onUpdateParallelLimit={vi.fn()}
        onUpdateSubdomain={vi.fn()}
        onUpdateApiKey={vi.fn()}
        onUpdateDescription={vi.fn()}
        onUpdateForSale={vi.fn()}
        onUpdateExpiration={vi.fn()}
        onUpdateAcl={vi.fn()}
      />,
    );

    const toggle = screen.getAllByLabelText("share.unlimited")[1];
    const parallelLimitInput = screen.getByDisplayValue("3");

    await user.click(toggle);
    expect(parallelLimitInput).toHaveValue(-1);
    expect(parallelLimitInput).toBeDisabled();

    await user.click(toggle);
    expect(parallelLimitInput).toHaveValue(3);
    expect(parallelLimitInput).toBeEnabled();
  });
});
