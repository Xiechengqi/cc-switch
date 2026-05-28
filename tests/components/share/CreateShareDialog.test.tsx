import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import {
  CreateShareDialog,
  deriveSubdomainFromEmail,
} from "@/components/share/CreateShareDialog";

function renderDialog(overrides: Partial<Record<string, unknown>> = {}) {
  const base: Record<string, unknown> = {
    open: true,
    onOpenChange: vi.fn(),
    defaultApp: "claude",
    ownerEmail: "owner@example.com",
    isSubmitting: false,
    tunnelConfig: { domain: "jptokenswitch.cc" },
    tunnelConfigSaving: false,
    onSaveTunnelConfig: vi.fn(),
    onSubmit: vi.fn(),
  };
  const props = Object.assign(base, overrides);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const rendered = render(<CreateShareDialog {...(props as any)} />);
  return { props, rendered };
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

    await user.click(screen.getByRole("button", { name: "share.create" }));

    // Confirm modal appears
    expect(screen.getByText(/确认使用默认设置创建/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "确认创建" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "owner@example.com",
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
  it("returns first 6 lowercase letters of the local part", () => {
    expect(deriveSubdomainFromEmail("johndoe@example.com")).toBe("johndo");
  });

  it("filters non-[a-z] characters and keeps the rest", () => {
    expect(deriveSubdomainFromEmail("alice42@example.com")).toBe("alice");
  });

  it("does not pad when result has 3-5 letters", () => {
    expect(deriveSubdomainFromEmail("ali@x.com")).toBe("ali");
    expect(deriveSubdomainFromEmail("alice@x.com")).toBe("alice");
  });

  it("pads with random [a-z] when result is shorter than 3", () => {
    const padded = deriveSubdomainFromEmail("ab@x.com");
    expect(padded).toHaveLength(6);
    expect(padded.startsWith("ab")).toBe(true);
    expect(padded.slice(2)).toMatch(/^[a-z]{4}$/);
  });

  it("pads from empty when local part has no letters", () => {
    const padded = deriveSubdomainFromEmail("123@x.com");
    expect(padded).toHaveLength(6);
    expect(padded).toMatch(/^[a-z]{6}$/);
  });
});
