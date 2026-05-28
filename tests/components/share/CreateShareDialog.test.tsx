import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { CreateShareDialog } from "@/components/share/CreateShareDialog";

describe("CreateShareDialog", () => {
  it("prefills singleton proxy share defaults", async () => {
    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={vi.fn()}
      />,
    );

    await waitFor(() =>
      expect(screen.getByDisplayValue("owner@example.com")).toBeInTheDocument(),
    );
    expect(screen.getByDisplayValue("100000")).toBeInTheDocument();
    expect(screen.getByDisplayValue("3")).toBeInTheDocument();
  });

  it("submits valid share payload", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={onSubmit}
      />,
    );

    await user.type(
      screen.getByLabelText("share.description"),
      "Team-facing proxy",
    );
    await user.click(screen.getByRole("button", { name: "share.create" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "owner@example.com",
          description: "Team-facing proxy",
          forSale: "No",
          autoStart: false,
          tokenLimit: 100000,
          parallelLimit: 3,
          expiresInSecs: 86400,
        }),
      ),
    );
  });

  it("submits start on launch when checked", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={onSubmit}
      />,
    );

    await user.click(screen.getByLabelText("share.autoStart"));
    await user.click(screen.getByRole("button", { name: "share.create" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          autoStart: true,
        }),
      ),
    );
  });

  it("lets owner email be edited and submits it as self-reported owner", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={onSubmit}
      />,
    );

    const ownerEmailInput = screen.getByLabelText("Owner Email");
    await user.clear(ownerEmailInput);
    await user.type(ownerEmailInput, "new-owner@example.com");
    await user.click(screen.getByRole("button", { name: "share.create" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          ownerEmail: "new-owner@example.com",
        }),
      ),
    );
  });

  it("locks token limit to -1 when unlimited is checked", async () => {
    const user = userEvent.setup();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={vi.fn()}
      />,
    );

    const tokenLimitInput = screen.getByLabelText("share.tokenLimit");
    expect(tokenLimitInput).toHaveValue(100000);

    await user.click(screen.getAllByLabelText("share.unlimited")[0]);

    expect(tokenLimitInput).toHaveValue(-1);
    expect(tokenLimitInput).toBeDisabled();
  });

  it("locks parallel limit to -1 when unlimited is checked", async () => {
    const user = userEvent.setup();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        ownerEmail="owner@example.com"
        isSubmitting={false}
        tunnelConfig={{ domain: "jptokenswitch.cc" }}
        tunnelConfigSaving={false}
        onSaveTunnelConfig={vi.fn()}
        onSubmit={vi.fn()}
      />,
    );

    const parallelLimitInput = screen.getByLabelText("share.parallelLimit");
    expect(parallelLimitInput).toHaveValue(3);

    const unlimitedToggles = screen.getAllByLabelText("share.unlimited");
    await user.click(unlimitedToggles[1]);

    expect(parallelLimitInput).toHaveValue(-1);
    expect(parallelLimitInput).toBeDisabled();
  });
});
