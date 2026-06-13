import { describe, expect, it } from "vitest";
import { buildCreateShareAccessPayload } from "./CreateShareDialog";

const emptyShareToByApp = {
  claude: [],
  codex: [],
  gemini: [],
};

describe("buildCreateShareAccessPayload", () => {
  it("delegates share-market sales to the default app when no provider binding is selected", () => {
    const payload = buildCreateShareAccessPayload({
      forSale: "Yes",
      saleMarketKind: "share",
      marketAccessMode: "all",
      fixedBindings: {},
      dynamicApps: [],
      shareToEmailsByApp: emptyShareToByApp,
      selectedTokenMarketEmails: [],
      selectedShareMarketEmail: "router@jptokenswitch.cc",
      defaultShareApp: "codex",
    });

    expect(payload).toEqual({
      sharedWithEmails: ["router@jptokenswitch.cc"],
      marketAccessMode: "selected",
      saleMarketKind: "share",
      accessByApp: {
        codex: {
          sharedWithEmails: ["router@jptokenswitch.cc"],
          marketAccessMode: "selected",
        },
      },
      appSettings: {
        codex: {
          forSale: "Yes",
          saleMarketKind: "share",
          marketAccessMode: "selected",
          sharedWithEmails: ["router@jptokenswitch.cc"],
          tokenLimit: -1,
          parallelLimit: -1,
          expiresAt: "",
        },
      },
    });
  });

  it("delegates share-market sales to every selected app binding", () => {
    const payload = buildCreateShareAccessPayload({
      forSale: "Yes",
      saleMarketKind: "share",
      marketAccessMode: "selected",
      fixedBindings: { claude: "p1" },
      dynamicApps: ["gemini"],
      shareToEmailsByApp: {
        ...emptyShareToByApp,
        claude: ["buyer@example.com"],
      },
      selectedTokenMarketEmails: [],
      selectedShareMarketEmail: "router@jptokenswitch.cc",
      defaultShareApp: "codex",
    });

    expect(payload.sharedWithEmails).toEqual([
      "buyer@example.com",
      "router@jptokenswitch.cc",
    ]);
    expect(payload.accessByApp).toEqual({
      claude: {
        sharedWithEmails: ["buyer@example.com", "router@jptokenswitch.cc"],
        marketAccessMode: "selected",
      },
      gemini: {
        sharedWithEmails: ["router@jptokenswitch.cc"],
        marketAccessMode: "selected",
      },
    });
  });

  it("keeps token-market all access as a market mode without per-app emails", () => {
    const payload = buildCreateShareAccessPayload({
      forSale: "Yes",
      saleMarketKind: "token",
      marketAccessMode: "all",
      fixedBindings: { claude: "p1" },
      dynamicApps: [],
      shareToEmailsByApp: emptyShareToByApp,
      selectedTokenMarketEmails: ["usage@example.com"],
      selectedShareMarketEmail: "",
      defaultShareApp: "claude",
    });

    expect(payload).toEqual({
      sharedWithEmails: [],
      marketAccessMode: "all",
      saleMarketKind: "token",
      accessByApp: undefined,
    });
  });
});
