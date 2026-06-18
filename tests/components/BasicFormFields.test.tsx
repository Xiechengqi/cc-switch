import { render, screen } from "@testing-library/react";
import type { PropsWithChildren } from "react";
import { useForm } from "react-hook-form";
import { describe, expect, it } from "vitest";
import { BasicFormFields } from "@/components/providers/forms/BasicFormFields";
import { Form } from "@/components/ui/form";
import type { ProviderFormData } from "@/lib/schemas/provider";

const FormShell = ({ children }: PropsWithChildren) => {
  const form = useForm<ProviderFormData>({
    defaultValues: {
      name: "Cursor API Key",
      websiteUrl: "https://cursor.com/dashboard/cloud-agents",
      notes: "",
      settingsConfig: "{}",
      icon: "",
      iconColor: "",
    },
  });

  return (
    <Form {...form}>
      <BasicFormFields form={form} isNameReadOnly isWebsiteUrlReadOnly />
      {children}
    </Form>
  );
};

describe("BasicFormFields", () => {
  it("locks provider name and website URL when requested", () => {
    render(<FormShell />);

    expect(screen.getByLabelText("provider.name")).toHaveAttribute("readonly");
    expect(screen.getByLabelText("provider.websiteUrl")).toHaveAttribute(
      "readonly",
    );
  });
});
