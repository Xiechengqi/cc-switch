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
        isSubmitting={false}
        onSubmit={vi.fn()}
      />,
    );

    await waitFor(() =>
      expect(screen.getByDisplayValue("Proxy Share")).toBeInTheDocument(),
    );
    expect(screen.getByDisplayValue("100000")).toBeInTheDocument();
  });

  it("submits valid share payload", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();

    render(
      <CreateShareDialog
        open={true}
        onOpenChange={vi.fn()}
        defaultApp="claude"
        isSubmitting={false}
        onSubmit={onSubmit}
      />,
    );

    await user.clear(screen.getByLabelText("share.name"));
    await user.type(screen.getByLabelText("share.name"), "Manual Share");
    await user.click(screen.getByRole("button", { name: "share.create" }));

    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith(
        expect.objectContaining({
          name: "Manual Share",
          tokenLimit: 100000,
          expiresInSecs: 86400,
        }),
      ),
    );
  });
});
