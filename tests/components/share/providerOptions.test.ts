import { describe, expect, it } from "vitest";
import {
  buildProviderOption,
  formatProviderOptionLabel,
  getProviderAccountLabel,
} from "@/components/share/providerOptions";
import type { ManagedAuthStatus } from "@/lib/api/auth";
import type { Provider } from "@/types";

describe("share provider options", () => {
  it("uses managed account email for OAuth providers", () => {
    const provider: Provider = {
      id: "claude-oauth-provider",
      name: "Claude OAuth",
      settingsConfig: {},
      meta: {
        providerType: "claude_oauth",
        authBinding: {
          source: "managed_account",
          authProvider: "claude_oauth",
          accountId: "account-1",
        },
      },
    };
    const authStatus: ManagedAuthStatus = {
      provider: "claude_oauth",
      authenticated: true,
      default_account_id: "account-1",
      accounts: [
        {
          id: "account-1",
          provider: "claude_oauth",
          login: "claude-user",
          email: "claude-user@example.com",
          avatar_url: null,
          authenticated_at: 1,
          is_default: true,
          github_domain: "github.com",
        },
      ],
    };

    expect(
      buildProviderOption(provider, false, { claude_oauth: authStatus }).detail,
    ).toBe("claude-user@example.com");
    expect(getProviderAccountLabel(provider, { claude_oauth: authStatus })).toBe(
      "claude-user@example.com",
    );
  });

  it("uses request URL for non-account providers", () => {
    const provider: Provider = {
      id: "api-provider",
      name: "API Provider",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://api.example.com",
        },
      },
    };
    const option = buildProviderOption(provider, true);

    expect(option.detail).toBe("https://api.example.com");
    expect(getProviderAccountLabel(provider)).toBeNull();
    expect(formatProviderOptionLabel(option, "已被其他 share 绑定")).toBe(
      "API Provider · https://api.example.com · 已被其他 share 绑定",
    );
  });
});
