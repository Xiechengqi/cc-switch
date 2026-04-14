import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import { describe, it, expect, beforeEach } from "vitest";
import { SharePage } from "@/components/share";
import { createTestQueryClient } from "../../utils/testQueryClient";
import { setSettings, setShares } from "../../msw/state";

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
      portrDomain: "server.example.com",
    });
    setShares([
      {
        id: "share-1",
        name: "Alpha Share",
        shareToken: "token-1",
        appType: "proxy",
        providerId: null,
        apiKey: "",
        settingsConfig: null,
        tokenLimit: 1000,
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
});
