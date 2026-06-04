import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { describe, it, expect, beforeEach } from "vitest";
import { SharePage } from "@/components/share";
import { createTestQueryClient } from "../../utils/testQueryClient";
import { server } from "../../msw/server";
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

  it("saves client tunnel owner email directly", async () => {
    const user = userEvent.setup();
    let savedParams: Record<string, unknown> | null = null;
    server.use(
      http.post("http://tauri.local/get_client_tunnel", () =>
        HttpResponse.json({
          config: {
            ownerEmail: "client@example.com",
            subdomain: "app-client",
            enabled: true,
            autoStart: true,
            tunnelUrl: "https://app-client.example.com",
          },
          status: {
            info: null,
            lastError: null,
            requiresOwnerLogin: false,
          },
        }),
      ),
      http.post(
        "http://tauri.local/claim_client_tunnel",
        async ({ request }) => {
          const body = (await request.json()) as {
            params: Record<string, unknown>;
          };
          savedParams = body.params;
          return HttpResponse.json({
            config: {
              ownerEmail: body.params.ownerEmail,
              subdomain: body.params.subdomain,
              enabled: body.params.enabled,
              autoStart: body.params.autoStart,
              tunnelUrl: `https://${body.params.subdomain}.example.com`,
            },
            status: {
              info: null,
              lastError: null,
              requiresOwnerLogin: false,
            },
          });
        },
      ),
      http.post("http://tauri.local/list_share_markets", () =>
        HttpResponse.json([]),
      ),
    );

    renderPage();

    const ownerInput = await screen.findByDisplayValue("client@example.com");
    await user.clear(ownerInput);
    await user.type(ownerInput, "new-client@example.com");
    const panel = ownerInput.closest(".rounded-lg");
    expect(panel).not.toBeNull();
    await user.click(
      within(panel as HTMLElement).getByRole("button", { name: "保存" }),
    );

    await waitFor(() =>
      expect(savedParams).toEqual({
        ownerEmail: "new-client@example.com",
        subdomain: "app-client",
        enabled: true,
        autoStart: true,
      }),
    );
    expect(screen.queryByText("Change Owner Email")).not.toBeInTheDocument();
  });
});
