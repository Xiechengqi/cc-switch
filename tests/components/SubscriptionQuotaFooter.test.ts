import { describe, expect, it } from "vitest";
import {
  formatCompactTier,
  formatExpireDistance,
  formatQuotaSummary,
} from "@/components/SubscriptionQuotaFooter";
import type { SubscriptionQuota } from "@/types/subscription";

const now = Date.parse("2026-06-30T00:00:00Z");

describe("SubscriptionQuotaFooter formatters", () => {
  it("formats subscription expiration distance", () => {
    expect(formatExpireDistance("2026-07-12T00:00:00Z", now)).toBe(
      "expire in 12d",
    );
    expect(formatExpireDistance("2026-06-30T06:00:00Z", now)).toBe(
      "expire in 6h",
    );
    expect(formatExpireDistance("2026-06-29T23:59:00Z", now)).toBe("expired");
    expect(formatExpireDistance(null, now)).toBe("expire unknown");
  });

  it("formats compact quota tiers", () => {
    expect(
      formatCompactTier(
        {
          name: "five_hour",
          utilization: 42.2,
          resetsAt: "2026-06-30T02:30:00Z",
        },
        undefined,
        now,
      ),
    ).toBe("5h 42% 2h30m");

    expect(
      formatCompactTier(
        {
          name: "seven_day",
          utilization: 18,
          resetsAt: "2026-07-04T06:00:00Z",
        },
        undefined,
        now,
      ),
    ).toBe("7d 18% 4d6h");

    expect(
      formatCompactTier(
        {
          name: "cursor_credits",
          utilization: 10.575,
          resetsAt: "2026-07-12T00:00:00Z",
          used: 42.3,
          limit: 400,
          unit: "USD",
        },
        undefined,
        now,
      ),
    ).toBe("Usage $42.3/$400 11% 12d0h");
  });

  it("builds the OpenAI subscription summary line", () => {
    const quota: SubscriptionQuota = {
      tool: "codex_oauth",
      credentialStatus: "valid",
      credentialMessage: "ChatGPT Plus",
      subscription: {
        planType: "plus",
        planLabel: "ChatGPT Plus",
        expiresAt: "2026-07-12T00:00:00Z",
        expiresSource: "subscriptions_active_until",
        expiresKind: "subscription",
      },
      success: true,
      tiers: [],
      extraUsage: null,
      error: null,
      queriedAt: now,
    };

    expect(
      formatQuotaSummary(
        quota,
        [
          {
            name: "five_hour",
            utilization: 42,
            resetsAt: "2026-06-30T02:30:00Z",
          },
          {
            name: "seven_day",
            utilization: 18,
            resetsAt: "2026-07-04T06:00:00Z",
          },
        ],
        undefined,
        now,
      ),
    ).toBe("ChatGPT Plus · expire in 12d · 5h 42% 2h30m · 7d 18% 4d6h");
  });

  it("builds the Cursor subscription summary line", () => {
    const quota: SubscriptionQuota = {
      tool: "cursor_oauth",
      credentialStatus: "valid",
      credentialMessage: "Cursor Pro",
      subscription: {
        planLabel: "Cursor Pro",
        expiresAt: "2026-07-12T00:00:00Z",
        expiresSource: "cursor_dashboard.billingCycleEnd",
        expiresKind: "billing_period",
      },
      success: true,
      tiers: [],
      extraUsage: null,
      error: null,
      queriedAt: now,
    };

    expect(
      formatQuotaSummary(
        quota,
        [
          {
            name: "cursor_credits",
            utilization: 10.575,
            resetsAt: "2026-07-12T00:00:00Z",
            used: 42.3,
            limit: 400,
            unit: "USD",
          },
        ],
        undefined,
        now,
      ),
    ).toBe("Cursor Pro · expire in 12d · Usage $42.3/$400 11% 12d0h");
  });
});
