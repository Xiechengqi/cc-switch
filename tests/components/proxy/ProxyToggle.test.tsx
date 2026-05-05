import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { beforeEach, describe, expect, it } from "vitest";
import { ProxyToggle } from "@/components/proxy/ProxyToggle";
import { server } from "../../msw/server";
import { setEmailAuthStatus, setSettings, setShares } from "../../msw/state";
import { createTestQueryClient } from "../../utils/testQueryClient";

const TAURI_ENDPOINT = "http://tauri.local";

const renderToggle = () => {
  const client = createTestQueryClient();
  return render(
    <QueryClientProvider client={client}>
      <ProxyToggle activeApp="claude" />
    </QueryClientProvider>,
  );
};

describe("ProxyToggle share flow", () => {
  beforeEach(() => {
    setSettings({
      shareRouterDomain: "server.example.com",
    });
    setEmailAuthStatus({
      authenticated: true,
      email: "alpha@example.com",
      expiresAt: Date.now() / 1000 + 3600,
    });
    setShares([
      {
        id: "share-1",
        name: "Alpha Share",
        ownerEmail: "alpha@example.com",
        sharedWithEmails: [],
        forSale: "No",
        shareToken: "token-1",
        appType: "proxy",
        providerId: null,
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
        createdAt: "2026-04-01T00:00:00.000Z",
        lastUsedAt: null,
      },
    ]);
  });

  it("enables takeover directly when an active share already exists", async () => {
    const user = userEvent.setup();
    const takeoverCalls: Array<{ appType: string; enabled: boolean }> = [];
    server.use(
      http.post(`${TAURI_ENDPOINT}/get_proxy_status`, () =>
        HttpResponse.json({
          running: true,
          address: "127.0.0.1",
          port: 53000,
          active_connections: 0,
          total_requests: 0,
          success_requests: 0,
          failed_requests: 0,
          success_rate: 0,
          uptime_seconds: 0,
          current_provider: null,
          current_provider_id: null,
          last_request_at: null,
          last_error: null,
          failover_count: 0,
          active_targets: [],
        }),
      ),
      http.post(
        `${TAURI_ENDPOINT}/set_proxy_takeover_for_app`,
        async ({ request }) => {
          takeoverCalls.push(
            (await request.json()) as (typeof takeoverCalls)[number],
          );
          return HttpResponse.json(true);
        },
      ),
    );

    renderToggle();

    await user.click(await screen.findByRole("switch", { name: /开启分享/i }));

    await waitFor(() =>
      expect(takeoverCalls).toContainEqual({
        appType: "claude",
        enabled: true,
      }),
    );
  });

  it("asks to start a paused share before enabling takeover", async () => {
    const user = userEvent.setup();
    const takeoverCalls: Array<{ appType: string; enabled: boolean }> = [];
    let enabledShareId: string | null = null;
    setShares([
      {
        id: "share-1",
        name: "Alpha Share",
        ownerEmail: "alpha@example.com",
        sharedWithEmails: [],
        forSale: "No",
        shareToken: "token-1",
        appType: "proxy",
        providerId: null,
        apiKey: "",
        settingsConfig: null,
        tokenLimit: 1000,
        parallelLimit: 3,
        tokensUsed: 300,
        requestsCount: 2,
        expiresAt: "2026-05-01T00:00:00.000Z",
        subdomain: "alpha",
        tunnelUrl: null,
        status: "paused",
        createdAt: "2026-04-01T00:00:00.000Z",
        lastUsedAt: null,
      },
    ]);
    server.use(
      http.post(`${TAURI_ENDPOINT}/enable_share`, async ({ request }) => {
        const body = (await request.json()) as { shareId: string };
        enabledShareId = body.shareId;
        return HttpResponse.json({
          tunnelUrl: "https://share-1.example.test",
          subdomain: "alpha",
          remotePort: 443,
          healthy: true,
        });
      }),
      http.post(
        `${TAURI_ENDPOINT}/set_proxy_takeover_for_app`,
        async ({ request }) => {
          takeoverCalls.push(
            (await request.json()) as (typeof takeoverCalls)[number],
          );
          return HttpResponse.json(true);
        },
      ),
    );

    renderToggle();

    await user.click(await screen.findByRole("switch", { name: /开启分享/i }));
    expect(await screen.findByText("启动分享")).toBeInTheDocument();
    await user.click(screen.getByText("启动并开启分享"));

    await waitFor(() => expect(enabledShareId).toBe("share-1"));
    await waitFor(() =>
      expect(takeoverCalls).toContainEqual({
        appType: "claude",
        enabled: true,
      }),
    );
  });
});
