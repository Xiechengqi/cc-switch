import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createElement, type ReactNode } from "react";
import { beforeAll, describe, it, expect, vi } from "vitest";
import { ShareCard } from "@/components/share/ShareCard";
import type { PublicMarket, ShareRecord } from "@/lib/api";
import { createTestQueryClient } from "../../utils/testQueryClient";

vi.mock("@/components/share/ShareRequestLogTable", () => ({
  ShareRequestLogTable: () => null,
}));

beforeAll(() => {
  if (!HTMLElement.prototype.hasPointerCapture) {
    HTMLElement.prototype.hasPointerCapture = () => false;
  }
  if (!HTMLElement.prototype.setPointerCapture) {
    HTMLElement.prototype.setPointerCapture = () => undefined;
  }
  if (!HTMLElement.prototype.releasePointerCapture) {
    HTMLElement.prototype.releasePointerCapture = () => undefined;
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => undefined;
  }
});

const tunnelConfig = {
  domain: "127.0.0.1:8787",
} as const;

const baseShare: ShareRecord = {
  id: "share-1",
  name: "Demo Share",
  ownerEmail: "owner@example.com",
  sharedWithEmails: [],
  marketAccessMode: "selected",
  forSaleOfficialPricePercentByApp: {},
  forSale: "No",
  bindings: {},
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
  autoStart: false,
  createdAt: "2026-04-01T00:00:00.000Z",
  lastUsedAt: null,
};

const markets: PublicMarket[] = [
  {
    id: "market-1",
    displayName: "Alpha Market",
    email: "alpha@example.com",
    subdomain: "alpha",
    publicBaseUrl: "https://alpha.example.com",
    status: "active",
  },
  {
    id: "market-2",
    displayName: "Beta Market",
    email: "beta@example.com",
    subdomain: "beta",
    publicBaseUrl: "https://beta.example.com",
    status: "active",
  },
];

describe("ShareCard", () => {
  const renderShareCard = (ui: ReactNode) => {
    const client = createTestQueryClient();
    return render(createElement(QueryClientProvider, { client }, ui));
  };
  const createHandlers = () => ({
    onDelete: vi.fn(),
    onEnable: vi.fn(),
    onDisable: vi.fn(),
    onResetUsage: vi.fn(),
    onUpdateTokenLimit: vi.fn(),
    onUpdateParallelLimit: vi.fn(),
    onUpdateSubdomain: vi.fn(),
    onUpdateApiKey: vi.fn(),
    onUpdateDescription: vi.fn(),
    onUpdateForSale: vi.fn(),
    onUpdateShareSalePricing: vi.fn(),
    onUpdateExpiration: vi.fn(),
    onUpdateAutoStart: vi.fn(),
    onUpdateOwnerEmail: vi.fn(),
    onTransferOwner: vi.fn(),
    onUpdateAcl: vi.fn(),
    onUpdateProviderBinding: vi.fn(),
  });

  it("shows disable for active share even when tunnel is not configured", () => {
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={false}
        {...createHandlers()}
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
        {...createHandlers()}
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
        {...createHandlers()}
        onDisable={onDisable}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.disable" }));
    expect(onDisable).toHaveBeenCalledWith(baseShare);
  });

  it("disables delete while share is not closed", () => {
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...createHandlers()}
      />,
    );

    expect(screen.getByRole("button", { name: "share.delete" })).toBeDisabled();
  });

  it("allows deleting a closed share", async () => {
    const user = userEvent.setup();
    const onDelete = vi.fn();
    const closedShare = { ...baseShare, status: "paused" };
    renderShareCard(
      <ShareCard
        share={closedShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...createHandlers()}
        onDelete={onDelete}
      />,
    );

    const deleteButton = screen.getByRole("button", { name: "share.delete" });
    expect(deleteButton).toBeEnabled();
    await user.click(deleteButton);
    expect(onDelete).toHaveBeenCalledWith(closedShare);
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
        {...createHandlers()}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "share.disable" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "share.enable" })).toBeEnabled();
  });

  it("keeps editable fields read-only until edit is clicked", async () => {
    const user = userEvent.setup();
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...createHandlers()}
      />,
    );

    expect(screen.queryByDisplayValue("1000")).not.toBeInTheDocument();
    expect(screen.getByText(/500\/1000/)).toBeInTheDocument();
    expect(
      screen.getByText(/share\.settings|Settings|设置项/),
    ).toBeInTheDocument();
    expect(screen.getByText("share.expiresAt")).toBeInTheDocument();
    expect(screen.queryByText("share.editDescription")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "保存设置" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "share.edit" }));

    const saveButton = screen.getByRole("button", {
      name: "保存设置",
    });
    expect(saveButton).toBeInTheDocument();
    const tokenLimitInput = screen.getByDisplayValue("1000");
    expect(tokenLimitInput).toBeEnabled();
    expect(saveButton).toBeDisabled();
  });

  it("keeps save disabled when the only difference is timestamp formatting", async () => {
    const user = userEvent.setup();
    renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          expiresAt: "2026-05-01T00:00:00+00:00",
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...createHandlers()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));

    expect(screen.getByRole("button", { name: "保存设置" })).toBeDisabled();
  });

  it("enables save after a field changes and submits the dirty field", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{ ...baseShare, description: "old description" }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    const descriptionInput = screen.getByDisplayValue("old description");
    await user.clear(descriptionInput);
    await user.type(descriptionInput, "new description");

    const saveButton = screen.getByRole("button", { name: "保存设置" });
    expect(saveButton).toBeEnabled();
    await user.click(saveButton);

    expect(handlers.onUpdateDescription).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      "new description",
    );
  });

  it("edits owner email as a normal share field", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={baseShare}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    const ownerEmailInput = screen.getByDisplayValue("owner@example.com");
    await user.clear(ownerEmailInput);
    await user.type(ownerEmailInput, "new-owner@example.com");

    const saveButton = screen.getByRole("button", { name: "保存设置" });
    expect(saveButton).toBeEnabled();
    await user.click(saveButton);

    expect(handlers.onUpdateOwnerEmail).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      "new-owner@example.com",
    );
  });

  it("selects a market in edit mode and saves it through the share ACL", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{ ...baseShare, forSale: "Yes" }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        markets={markets}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    await user.click(
      screen.getByRole("combobox", { name: /Select market|选择 Market/i }),
    );
    await user.click(
      await screen.findByRole("option", { name: "Alpha Market" }),
    );

    expect(screen.getAllByText("Alpha Market").length).toBeGreaterThan(0);
    const saveButton = screen.getByRole("button", { name: "保存设置" });
    expect(saveButton).toBeEnabled();
    await user.click(saveButton);

    expect(handlers.onUpdateAcl).toHaveBeenCalledTimes(1);
    expect(handlers.onUpdateAcl).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      ["alpha@example.com"],
      "selected",
    );
  });

  it("selects dynamic all-markets access when All is chosen", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{ ...baseShare, forSale: "Yes" }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        markets={markets}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    await user.click(
      screen.getByRole("combobox", { name: /Select market|选择 Market/i }),
    );
    await user.click(await screen.findByRole("option", { name: "All" }));
    await user.click(screen.getByRole("button", { name: "保存设置" }));

    expect(handlers.onUpdateAcl).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      [],
      "all",
    );
  });

  it("allows dynamic all-markets access before any market is fetched", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{ ...baseShare, forSale: "Yes" }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        markets={[]}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    await user.click(
      screen.getByRole("combobox", { name: /Select market|选择 Market/i }),
    );
    await user.click(await screen.findByRole("option", { name: "All" }));
    await user.click(screen.getByRole("button", { name: "保存设置" }));

    expect(handlers.onUpdateAcl).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      [],
      "all",
    );
  });

  it("saves global model pricing on the share instead of the provider", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          forSale: "Yes",
          // P12：定价 row 只对已绑定 app 显示。本测试用 claude 价格，必须先有
          // claude binding，否则 EditShareDialog 不渲染该 row，下面 spinbutton
          // 也找不到。
          bindings: { claude: "test-provider" },
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        providerSalePricing={[
          {
            app: "claude",
            label: "Claude",
            providerName: "Claude Provider",
            percent: 20,
          },
        ]}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    const input = screen.getAllByRole("spinbutton")[0];
    await user.clear(input);
    await user.type(input, "15");
    await user.click(screen.getByRole("button", { name: "保存设置" }));

    expect(handlers.onUpdateShareSalePricing).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      { claude: 15 },
    );
  });

  it("keeps global model pricing edits during background rerenders", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    const providerSalePricing = [
      {
        app: "claude" as const,
        label: "Claude",
        providerName: "Claude Provider",
        percent: 20,
      },
    ];
    const { rerender } = renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          forSale: "Yes",
          bindings: { claude: "test-provider" },
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        providerSalePricing={providerSalePricing}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    const input = screen.getAllByRole("spinbutton")[0];
    await user.clear(input);
    await user.type(input, "15");

    rerender(
      <QueryClientProvider client={createTestQueryClient()}>
        <ShareCard
          share={{
            ...baseShare,
            forSale: "Yes",
            requestsCount: 4,
            bindings: { claude: "test-provider" },
          }}
          tunnelConfig={tunnelConfig}
          tunnelConfigured={true}
          providerSalePricing={[...providerSalePricing]}
          {...handlers}
        />
      </QueryClientProvider>,
    );

    expect(screen.getAllByRole("spinbutton")[0]).toHaveValue(15);
  });

  it("hides per-app pricing rows for apps the share has no binding for (P12)", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          forSale: "Yes",
          // 只绑 claude，codex/gemini 留空——它们的定价行必须不渲染。
          bindings: { claude: "test-provider" },
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        providerSalePricing={[
          { app: "claude", label: "Claude", providerName: "Claude P", percent: 20 },
          { app: "codex", label: "Codex", providerName: "Codex P", percent: 30 },
          { app: "gemini", label: "Gemini", providerName: "Gemini P", percent: 40 },
        ]}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    // 定价行用 min=1 max=100 的 number input；未绑定的 codex/gemini 不该有 row。
    const pricingInputs = document.querySelectorAll<HTMLInputElement>(
      'input[type="number"][min="1"][max="100"]',
    );
    expect(pricingInputs.length).toBe(1);
  });

  it("restores market selection to the default while preserving Share To emails", async () => {
    const user = userEvent.setup();
    const handlers = createHandlers();
    renderShareCard(
      <ShareCard
        share={{
          ...baseShare,
          forSale: "Yes",
          sharedWithEmails: ["friend@example.com", "alpha@example.com"],
        }}
        tunnelConfig={tunnelConfig}
        tunnelConfigured={true}
        markets={markets}
        {...handlers}
      />,
    );

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    await user.click(screen.getByRole("button", { name: /Restore|还原/ }));
    await user.click(screen.getByRole("button", { name: "保存设置" }));

    expect(handlers.onUpdateAcl).toHaveBeenCalledTimes(1);
    expect(handlers.onUpdateAcl).toHaveBeenCalledWith(
      expect.objectContaining({ id: "share-1" }),
      ["friend@example.com"],
      "selected",
    );
  });
});
