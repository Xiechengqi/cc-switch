import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { TunnelConfigPanel } from "@/components/share/TunnelConfigPanel";

describe("TunnelConfigPanel", () => {
  it("saves edited tunnel config", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();

    render(
      <TunnelConfigPanel
        initialConfig={{
          domain: "",
        }}
        tunnelConfigured={false}
        isSaving={false}
        onSave={onSave}
      />,
    );

    await user.type(screen.getByLabelText("share.tunnel.domain"), "example.com");
    await user.click(screen.getByRole("button", { name: "common.save" }));

    expect(onSave).toHaveBeenCalledWith({
      domain: "example.com",
    });
  });
});
