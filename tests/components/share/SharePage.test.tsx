import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, beforeEach } from "vitest";
import { SharePage } from "@/components/share";
import { createTestQueryClient } from "../../utils/testQueryClient";
import {
  setEmailAuthStatus,
  setSettings,
  setShares,
  listShares,
} from "../../msw/state";

const renderPage = () => {
  const client = createTestQueryClient();
  return render(
    <QueryClientProvider client={client}>
      <SharePage defaultApp="claude" />
    </QueryClientProvider>,
  );
};

describe("SharePage", () => {
  beforeEach(() => {
    setSettings({
      shareRouterDomain: "server.example.com",
    });
    setEmailAuthStatus({
      authenticated: false,
      email: null,
      expiresAt: null,
    });
    setShares([
      {
        id: "share-1",
        name: "Alpha Share",
        ownerEmail: "alpha@example.com",
        sharedWithEmails: [],
        marketAccessMode: "selected",
        forSaleOfficialPricePercentByApp: {},
        forSale: "No",
        shareToken: "token-1",
        bindings: {},
        apiKey: "",
        settingsConfig: null,
        tokenLimit: 1000,
        parallelLimit: 3,
        tokensUsed: 300,
        requestsCount: 2,
        expiresAt: "2026-05-01T00:00:00.000Z",
        subdomain: "alpha",
        tunnelUrl: "https://share-1.example.test",
        status: "active",
        autoStart: false,
        createdAt: "2026-04-01T00:00:00.000Z",
        lastUsedAt: null,
      },
    ]);
  });

  it("renders the single share card", async () => {
    renderPage();

    await waitFor(() =>
      expect(screen.getByText("Alpha Share")).toBeInTheDocument(),
    );
    expect(screen.getByText("Alpha Share")).toBeInTheDocument();
  });

  it("shows share creation entry when no share is bound and user is unauthenticated", async () => {
    setShares([]);

    renderPage();

    // P5.7 之后空状态用 share.emptyCta（"创建第一个分享"）作为 CTA；
    // 老路径的 share.create 是列表头部 toolbar 按钮，仅在已有 share 时显示。
    await waitFor(() =>
      expect(screen.getByText("share.emptyCta")).toBeInTheDocument(),
    );
    expect(screen.queryByText("Alpha Share")).not.toBeInTheDocument();
    expect(screen.queryByText("Login Share Owner")).not.toBeInTheDocument();
  });

  it("changes owner email through the normal edit form", async () => {
    const user = userEvent.setup();
    renderPage();

    await screen.findByText("Alpha Share");
    expect(screen.queryByText("重新验证 Owner 邮箱")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "share.edit" }));
    const ownerEmailInput = screen.getByDisplayValue("alpha@example.com");
    await user.clear(ownerEmailInput);
    await user.type(ownerEmailInput, "new-owner@example.com");
    await user.click(screen.getByRole("button", { name: "保存设置" }));

    await waitFor(() =>
      expect(listShares()[0]?.ownerEmail).toBe("new-owner@example.com"),
    );
  });
});
