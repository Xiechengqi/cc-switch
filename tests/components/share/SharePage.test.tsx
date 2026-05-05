import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { describe, it, expect, beforeEach } from "vitest";
import { SharePage } from "@/components/share";
import { server } from "../../msw/server";
import { createTestQueryClient } from "../../utils/testQueryClient";
import {
  setEmailAuthSession,
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
    setEmailAuthSession({
      authenticated: false,
      user: null,
      expiresAt: null,
      installationOwnerEmail: null,
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

  it("renders the single share card", async () => {
    renderPage();

    await waitFor(() =>
      expect(screen.getByText("Alpha Share")).toBeInTheDocument(),
    );
    expect(screen.getByText("Alpha Share")).toBeInTheDocument();
  });

  it("shows only share owner login entry when no share is bound and user is unauthenticated", async () => {
    setShares([]);

    renderPage();

    await waitFor(() =>
      expect(screen.getByText("Login Share Owner")).toBeInTheDocument(),
    );
    expect(screen.getByText("Login Share Owner")).toBeInTheDocument();
    expect(screen.queryByText("Alpha Share")).not.toBeInTheDocument();
    expect(screen.queryByText("Router & Tunnel")).not.toBeInTheDocument();
  });

  it("opens owner login dialog and sends email code through selected router", async () => {
    setShares([]);
    setSettings({
      shareRouterDomain: "jptokenswitch.cc",
    });
    let requestBody: { routerDomain?: string; email?: string } | null = null;
    server.use(
      http.post("http://tauri.local/email_auth_request_code", async ({ request }) => {
        requestBody = (await request.json()) as typeof requestBody;
        return HttpResponse.json({
          ok: true,
          cooldownSecs: 60,
          maskedDestination: requestBody?.email ?? "***",
        });
      }),
    );

    renderPage();

    fireEvent.click(await screen.findByText("Login Share Owner"));
    expect(screen.getByText("Share Owner Login")).toBeInTheDocument();
    expect(screen.getByText(/1\. Router/)).toHaveClass("text-foreground");
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("Continue"));
    const emailInput = await screen.findByLabelText("Email");
    fireEvent.change(emailInput, {
      target: { value: "owner@example.com" },
    });
    fireEvent.click(screen.getByText("发送验证码"));

    await waitFor(() =>
      expect(requestBody).toEqual({
        routerDomain: "jptokenswitch.cc",
        email: "owner@example.com",
      }),
    );
    expect(screen.getByLabelText("Verification Code")).toBeInTheDocument();
  });

  it("reverifies the locked share owner email from the share page", async () => {
    const user = userEvent.setup();
    let requestBody: { routerDomain?: string; email?: string } | null = null;
    server.use(
      http.post("http://tauri.local/email_auth_request_code", async ({ request }) => {
        requestBody = (await request.json()) as typeof requestBody;
        return HttpResponse.json({
          ok: true,
          cooldownSecs: 60,
          maskedDestination: requestBody?.email ?? "***",
        });
      }),
    );

    renderPage();

    await user.click(await screen.findByText("重新验证 Owner 邮箱"));
    await user.click(screen.getByText("Continue"));
    const emailInput = await screen.findByLabelText("Email");
    expect(emailInput).toHaveValue("alpha@example.com");
    expect(emailInput).toBeDisabled();
    await user.click(screen.getByText("发送验证码"));

    await waitFor(() =>
      expect(requestBody).toEqual({
        routerDomain: "server.example.com",
        email: "alpha@example.com",
      }),
    );
    expect(screen.getByLabelText("Verification Code")).toBeInTheDocument();
  });

  it("changes owner email by verifying the new owner email", async () => {
    const user = userEvent.setup();
    setEmailAuthStatus({
      authenticated: true,
      email: "alpha@example.com",
      expiresAt: Date.now() / 1000 + 3600,
    });
    setEmailAuthSession({
      authenticated: true,
      user: {
        id: "email-user-1",
        email: "alpha@example.com",
      },
      expiresAt: new Date(Date.now() + 3600 * 1000).toISOString(),
      installationOwnerEmail: "alpha@example.com",
    });
    let ownerCodeRequest: {
      routerDomain?: string;
      currentEmail?: string;
      newEmail?: string;
    } | null = null;
    server.use(
      http.post(
        "http://tauri.local/email_auth_request_owner_change_code",
        async ({ request }) => {
          ownerCodeRequest = (await request.json()) as typeof ownerCodeRequest;
          return HttpResponse.json({
            ok: true,
            cooldownSecs: 60,
            maskedDestination: ownerCodeRequest?.newEmail ?? "***",
          });
        },
      ),
    );

    renderPage();

    await screen.findByText("Alpha Share");
    await user.click(screen.getByRole("button", { name: "Change Owner Email" }));
    await waitFor(() =>
      expect(screen.getAllByText("Change Owner Email").length).toBeGreaterThan(
        1,
      ),
    );
    expect(screen.getByText("alpha@example.com")).toBeInTheDocument();

    await user.click(screen.getByText("Continue"));
    await user.type(
      await screen.findByLabelText("New owner email"),
      "new-owner@example.com",
    );
    await user.click(screen.getByText("发送验证码"));
    await waitFor(() =>
      expect(screen.getByText(/new-owner@example.com/)).toBeInTheDocument(),
    );
    expect(ownerCodeRequest).toEqual({
      routerDomain: "server.example.com",
      currentEmail: "alpha@example.com",
      newEmail: "new-owner@example.com",
    });

    await user.type(await screen.findByLabelText("Verification Code"), "123456");
    await user.click(screen.getByText("Change Owner"));

    await waitFor(() =>
      expect(listShares()[0]?.ownerEmail).toBe("new-owner@example.com"),
    );
  });
});
