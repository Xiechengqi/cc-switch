import { describe, expect, it } from "vitest";
import type { Provider, ProviderMeta } from "@/types";
import {
  canTestProvider,
  getProviderQuotaSource,
  hasManagedAuthBinding,
  isManagedOauthProvider,
  isOfficialBlockedByProxyTakeover,
  isCodexOfficialWithManagedAuth,
  mergeProviderMeta,
} from "@/utils/providerMetaUtils";

const buildEndpoint = (url: string) => ({
  url,
  addedAt: 1,
});

describe("mergeProviderMeta", () => {
  it("returns undefined when no initial meta and no endpoints", () => {
    expect(mergeProviderMeta(undefined, null)).toBeUndefined();
    expect(mergeProviderMeta(undefined, undefined)).toBeUndefined();
  });

  it("creates meta when endpoints are provided for new provider", () => {
    const result = mergeProviderMeta(undefined, {
      "https://example.com": buildEndpoint("https://example.com"),
    });

    expect(result).toEqual({
      custom_endpoints: {
        "https://example.com": buildEndpoint("https://example.com"),
      },
    });
  });

  it("overrides custom endpoints but preserves other fields", () => {
    const initial: ProviderMeta = {
      usage_script: {
        enabled: true,
        language: "javascript",
        code: "console.log(1);",
      },
      custom_endpoints: {
        "https://old.com": buildEndpoint("https://old.com"),
      },
    };

    const result = mergeProviderMeta(initial, {
      "https://new.com": buildEndpoint("https://new.com"),
    });

    expect(result).toEqual({
      usage_script: initial.usage_script,
      custom_endpoints: {
        "https://new.com": buildEndpoint("https://new.com"),
      },
    });
  });

  it("removes custom endpoints when result is empty but keeps other meta", () => {
    const initial: ProviderMeta = {
      usage_script: {
        enabled: true,
        language: "javascript",
        code: "console.log(1);",
      },
      custom_endpoints: {
        "https://example.com": buildEndpoint("https://example.com"),
      },
    };

    const result = mergeProviderMeta(initial, null);

    expect(result).toEqual({
      usage_script: initial.usage_script,
    });
  });

  it("returns undefined when removing last field", () => {
    const initial: ProviderMeta = {
      custom_endpoints: {
        "https://example.com": buildEndpoint("https://example.com"),
      },
    };

    expect(mergeProviderMeta(initial, null)).toBeUndefined();
  });
});

describe("hasManagedAuthBinding", () => {
  it("returns true only when managed account binding has a non-empty account id", () => {
    expect(
      hasManagedAuthBinding(
        {
          authBinding: {
            source: "managed_account",
            authProvider: "codex_oauth",
            accountId: "acct-1",
          },
        },
        "codex_oauth",
      ),
    ).toBe(true);

    expect(
      hasManagedAuthBinding(
        {
          authBinding: {
            source: "managed_account",
            authProvider: "codex_oauth",
          },
        },
        "codex_oauth",
      ),
    ).toBe(false);
  });
});

describe("isCodexOfficialWithManagedAuth", () => {
  it("detects managed Codex official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: {
        authBinding: {
          source: "managed_account",
          authProvider: "codex_oauth",
          accountId: "acct-1",
        },
      },
    };

    expect(isCodexOfficialWithManagedAuth(provider)).toBe(true);
  });

  it("rejects unbound official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: undefined,
    };

    expect(isCodexOfficialWithManagedAuth(provider)).toBe(false);
  });
});

describe("getProviderQuotaSource", () => {
  it("uses codex oauth quota source for managed Codex official", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: {
        authBinding: {
          source: "managed_account",
          authProvider: "codex_oauth",
          accountId: "acct-1",
        },
      },
    };

    expect(getProviderQuotaSource(provider, "codex")).toBe("codex_oauth");
  });

  it("uses claude oauth quota source for Claude official oauth providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: {
        providerType: "claude_oauth",
      },
    };

    expect(getProviderQuotaSource(provider, "claude")).toBe("claude_oauth");
  });

  it("uses copilot quota source for github copilot providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "third_party",
      meta: {
        providerType: "github_copilot",
      },
    };

    expect(getProviderQuotaSource(provider, "claude")).toBe("copilot");
  });

  it("falls back to official quota source for plain official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: undefined,
    };

    expect(getProviderQuotaSource(provider, "gemini")).toBe("official");
  });

  it("returns none for non-official providers without oauth quota source", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "third_party",
      meta: undefined,
    };

    expect(getProviderQuotaSource(provider, "codex")).toBe("none");
  });
});

describe("isManagedOauthProvider", () => {
  it("detects managed oauth provider types", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: { providerType: "claude_oauth" },
    };

    expect(isManagedOauthProvider(provider, "claude")).toBe(true);
  });

  it("detects managed Codex official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: {
        authBinding: {
          source: "managed_account",
          authProvider: "codex_oauth",
          accountId: "acct-1",
        },
      },
    };

    expect(isManagedOauthProvider(provider, "codex")).toBe(true);
  });
});

describe("isOfficialBlockedByProxyTakeover", () => {
  it("blocks plain official providers during proxy takeover", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: undefined,
    };

    expect(isOfficialBlockedByProxyTakeover(provider, "claude", true)).toBe(
      true,
    );
  });

  it("does not block managed official oauth providers during proxy takeover", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: { providerType: "claude_oauth" },
    };

    expect(isOfficialBlockedByProxyTakeover(provider, "claude", true)).toBe(
      false,
    );
  });
});

describe("canTestProvider", () => {
  it("allows Claude OAuth providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: { providerType: "claude_oauth" },
    };

    expect(canTestProvider(provider, "claude")).toBe(true);
  });

  it("allows managed Codex official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: {
        authBinding: {
          source: "managed_account",
          authProvider: "codex_oauth",
          accountId: "acct-1",
        },
      },
    };

    expect(canTestProvider(provider, "codex")).toBe(true);
  });

  it("rejects plain official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "official",
      meta: undefined,
    };

    expect(canTestProvider(provider, "gemini")).toBe(false);
  });

  it("rejects Copilot and Codex OAuth provider types", () => {
    expect(
      canTestProvider(
        {
          category: "third_party",
          meta: { providerType: "github_copilot" },
        },
        "claude",
      ),
    ).toBe(false);
    expect(
      canTestProvider(
        {
          category: "third_party",
          meta: { providerType: "codex_oauth" },
        },
        "claude",
      ),
    ).toBe(false);
  });

  it("allows normal non-official providers", () => {
    const provider: Pick<Provider, "category" | "meta"> = {
      category: "third_party",
      meta: undefined,
    };

    expect(canTestProvider(provider, "codex")).toBe(true);
  });
});
