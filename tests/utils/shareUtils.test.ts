import { describe, expect, it } from "vitest";
import {
  getShareDisplayStatus,
  getShareTunnelRuntimeStatus,
  isShareActionAllowed,
} from "@/utils/shareUtils";
import type { ShareRecord, TunnelInfo } from "@/lib/api";

const baseShare: ShareRecord = {
  id: "share-1",
  name: "Demo",
  forSale: "No",
  shareToken: "token",
  appType: "proxy",
  providerId: null,
  apiKey: "key",
  settingsConfig: null,
  tokenLimit: 1000,
  tokensUsed: 0,
  requestsCount: 0,
  expiresAt: "2099-12-31T23:59:59Z",
  subdomain: "demo",
  tunnelUrl: "https://demo.example.com",
  status: "active",
  createdAt: "2026-01-01T00:00:00Z",
  lastUsedAt: null,
};

const healthyTunnel: TunnelInfo = {
  tunnelUrl: "https://demo.example.com",
  subdomain: "demo",
  remotePort: 12345,
  healthy: true,
};

describe("share utils", () => {
  describe("display status", () => {
    it("shows closed for paused shares even with stale healthy tunnel status", () => {
      expect(
        getShareDisplayStatus(
          { ...baseShare, status: "paused", tunnelUrl: null },
          true,
          healthyTunnel,
        ),
      ).toBe("closed");
    });

    it("shows sharing for active shares with a healthy tunnel", () => {
      expect(getShareDisplayStatus(baseShare, true, healthyTunnel)).toBe(
        "sharing",
      );
    });

    it("shows connecting for active shares with an unhealthy tunnel", () => {
      expect(
        getShareDisplayStatus(baseShare, true, {
          ...healthyTunnel,
          healthy: false,
        }),
      ).toBe("connecting");
    });

    it("shows connecting for active shares with a tunnel url while health is unknown", () => {
      expect(getShareDisplayStatus(baseShare, true, null)).toBe("connecting");
    });

    it("shows connection_error for active shares without a tunnel url", () => {
      expect(
        getShareDisplayStatus({ ...baseShare, tunnelUrl: null }, true, null),
      ).toBe("connection_error");
    });

    it("shows business terminal statuses before tunnel configuration", () => {
      expect(
        getShareDisplayStatus({ ...baseShare, status: "expired" }, false, null),
      ).toBe("expired");
      expect(
        getShareDisplayStatus(
          { ...baseShare, status: "exhausted" },
          false,
          healthyTunnel,
        ),
      ).toBe("exhausted");
    });

    it("shows not_configured for active shares without tunnel configuration", () => {
      expect(getShareDisplayStatus(baseShare, false, healthyTunnel)).toBe(
        "not_configured",
      );
    });
  });

  it("ignores stale running tunnel status when share is paused", () => {
    const pausedShare = { ...baseShare, status: "paused", tunnelUrl: null };

    expect(getShareTunnelRuntimeStatus(pausedShare, healthyTunnel)).toBe(
      "unknown",
    );
    expect(
      isShareActionAllowed(pausedShare, "disable", true, healthyTunnel),
    ).toBe(false);
    expect(
      isShareActionAllowed(pausedShare, "enable", true, healthyTunnel),
    ).toBe(true);
  });

  it("allows disabling only active shares", () => {
    expect(isShareActionAllowed(baseShare, "disable", true, null)).toBe(true);
    expect(
      isShareActionAllowed(
        { ...baseShare, status: "paused" },
        "disable",
        true,
        null,
      ),
    ).toBe(false);
  });
});
