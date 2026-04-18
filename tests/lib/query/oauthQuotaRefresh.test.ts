import { describe, expect, it } from "vitest";
import {
  DEFAULT_OAUTH_QUOTA_REFRESH_INTERVAL_MINUTES,
  getOauthQuotaRefreshIntervalMinutes,
  getOauthQuotaRefreshIntervalMs,
} from "@/lib/query/oauthQuotaRefresh";
import type { Settings } from "@/types";

describe("oauth quota refresh interval", () => {
  it("defaults to 5 minutes", () => {
    expect(getOauthQuotaRefreshIntervalMinutes(undefined)).toBe(
      DEFAULT_OAUTH_QUOTA_REFRESH_INTERVAL_MINUTES,
    );
    expect(getOauthQuotaRefreshIntervalMs(undefined)).toBe(5 * 60 * 1000);
  });

  it("uses configured integer minutes", () => {
    expect(
      getOauthQuotaRefreshIntervalMinutes({
        oauthQuotaRefreshIntervalMinutes: 12,
      } as Settings),
    ).toBe(12);
    expect(
      getOauthQuotaRefreshIntervalMs({
        oauthQuotaRefreshIntervalMinutes: 12,
      } as Settings),
    ).toBe(12 * 60 * 1000);
  });

  it("floors decimals and clamps invalid low values to 1 minute", () => {
    expect(
      getOauthQuotaRefreshIntervalMinutes({
        oauthQuotaRefreshIntervalMinutes: 2.9,
      } as Settings),
    ).toBe(2);
    expect(
      getOauthQuotaRefreshIntervalMinutes({
        oauthQuotaRefreshIntervalMinutes: 0,
      } as Settings),
    ).toBe(1);
  });
});
