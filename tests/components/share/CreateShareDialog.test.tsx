import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import {
  CreateShareDialog,
  deriveSubdomainFromEmail,
} from "@/components/share/CreateShareDialog";

const TEST_PROVIDERS = [
  { id: "test-provider-1", name: "Test Provider 1", disabled: false },
];

function renderDialog(overrides: Partial<Record<string, unknown>> = {}) {
  const base: Record<string, unknown> = {
    open: true,
    onOpenChange: vi.fn(),
    defaultApp: "claude",
    ownerEmail: "owner@example.com",
    isSubmitting: false,
    tunnelConfig: { domain: "jptokenswitch.cc" },
    tunnelConfigSaving: false,
    // P5.1 引入 `providers` 为必填。默认带一条可选 provider，需要 submit 的
    // 用例可以走 selectProvider helper 选中它。
    providers: TEST_PROVIDERS,
    onSaveTunnelConfig: vi.fn(),
    onSubmit: vi.fn(),
  };
  const props = Object.assign(base, overrides);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const rendered = render(<CreateShareDialog {...(props as any)} />);
  return { props, rendered };
}

// 从 Radix Select 里点选第一条可用 provider。Radix Select 在 jsdom 下需要靠
// setupGlobals 的 pointer-capture polyfill 才点得开。
async function selectProvider(
  user: ReturnType<typeof userEvent.setup>,
  providerName: string = TEST_PROVIDERS[0]!.name,
) {
  const trigger = document.getElementById("share-create-provider");
  if (!trigger) throw new Error("Provider Select trigger not found");
  await user.click(trigger);
  const option = await screen.findByRole("option", { name: providerName });
  await user.click(option);
}

describe("CreateShareDialog", () => {
  it("collapses advanced settings by default", () => {
    renderDialog();
    // Advanced controls (e.g. ForSale select, autoStart checkbox) are hidden.
    expect(
      screen.queryByLabelText("share.autoStart"),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("share.tokenLimit")).not.toBeInTheDocument();
    expect(screen.queryByText(/将以默认设置创建/)).toBeInTheDocument();
  });

  it("requires confirmation when creating with defaults", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderDialog({ onSubmit });

    await selectProvider(user);
    await user.click(screen.getByRole("button", { name: "share.create" }));

    // Confirm modal appears
    expect(screen.getByText(/确认使用默认设置创建/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "确认创建" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "owner@example.com",
          providerId: TEST_PROVIDERS[0]!.id,
          forSale: "Yes",
          autoStart: true,
          tokenLimit: -1,
          parallelLimit: -1,
        }),
        expect.objectContaining({
          marketAccessMode: "all",
          sharedWithEmails: [],
        }),
      ),
    );
  });

  it("submits directly without confirmation when advanced is expanded", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderDialog({ onSubmit });

    await selectProvider(user);
    await user.click(
      screen.getByRole("button", { name: /高级设置|advanced/i }),
    );

    await user.click(screen.getByRole("button", { name: "share.create" }));

    // No confirm modal interaction needed
    expect(screen.queryByText(/确认使用默认设置创建/)).not.toBeInTheDocument();
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "owner@example.com",
          providerId: TEST_PROVIDERS[0]!.id,
          forSale: "Yes",
        }),
        expect.objectContaining({ marketAccessMode: "all" }),
      ),
    );
  });

  it("toggling unlimited inside advanced flips token limit", async () => {
    const user = userEvent.setup();
    renderDialog();

    await user.click(
      screen.getByRole("button", { name: /高级设置|advanced/i }),
    );

    const tokenLimitInput = screen.getByLabelText("share.tokenLimit");
    expect(tokenLimitInput).toHaveValue(-1);
    expect(tokenLimitInput).toBeDisabled();

    // Click the unlimited checkbox to disable the unlimited mode
    await user.click(screen.getAllByLabelText("share.unlimited")[0]);
    expect(tokenLimitInput).toHaveValue(100000);
    expect(tokenLimitInput).not.toBeDisabled();
  });

  it("lets owner email be edited and submits it as self-reported owner", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderDialog({ onSubmit });

    await selectProvider(user);
    const ownerEmailInput = screen.getByLabelText("Owner Email");
    await user.clear(ownerEmailInput);
    await user.type(ownerEmailInput, "new-owner@example.com");

    await user.click(screen.getByRole("button", { name: "share.create" }));
    await user.click(screen.getByRole("button", { name: "确认创建" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "new-owner@example.com",
        }),
        expect.anything(),
      ),
    );
  });
});

describe("deriveSubdomainFromEmail", () => {
  // 形态：`{email-prefix}-{base36-timestamp-suffix}`。
  // 多 share 模式下时间戳后缀保证同 owner 连续创建不撞。
  const SUFFIX_RE = /-[0-9a-z]{5}$/;
  const FULL_RE = /^[a-z][a-z]{0,4}-[0-9a-z]{5}$/;

  it("takes the first 5 lowercase letters of the local part as prefix", () => {
    const subdomain = deriveSubdomainFromEmail("johndoe@example.com");
    expect(subdomain.startsWith("johnd-")).toBe(true);
    expect(subdomain).toMatch(SUFFIX_RE);
  });

  it("filters non-[a-z] characters before truncating the prefix", () => {
    const subdomain = deriveSubdomainFromEmail("alice42@example.com");
    expect(subdomain.startsWith("alice-")).toBe(true);
    expect(subdomain).toMatch(SUFFIX_RE);
  });

  it("keeps short prefixes as-is and still appends a timestamp suffix", () => {
    expect(deriveSubdomainFromEmail("ali@x.com")).toMatch(/^ali-[0-9a-z]{5}$/);
    expect(deriveSubdomainFromEmail("ab@x.com")).toMatch(/^ab-[0-9a-z]{5}$/);
  });

  it("falls back to `s` prefix when local part has no [a-z] letters", () => {
    const subdomain = deriveSubdomainFromEmail("123@x.com");
    expect(subdomain).toMatch(/^s-[0-9a-z]{5}$/);
  });

  it("produces a different result on a later call (timestamp tiebreaker)", async () => {
    const first = deriveSubdomainFromEmail("alice@x.com");
    // 等待一毫秒确保 Date.now() 进位（base36 末 5 位粒度极高，足以变化）。
    await new Promise((resolve) => setTimeout(resolve, 2));
    const second = deriveSubdomainFromEmail("alice@x.com");
    expect(first).not.toBe(second);
    expect(first).toMatch(FULL_RE);
    expect(second).toMatch(FULL_RE);
  });
});
