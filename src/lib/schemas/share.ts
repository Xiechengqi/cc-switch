import { z } from "zod";

export const createShareSchema = z.object({
  description: z
    .string()
    .trim()
    .optional()
    .transform((value) => value ?? "")
    .refine(
      (value) => value.length <= 200,
      "share.validation.descriptionTooLong",
    ),
  forSale: z.enum(["Yes", "No", "Free"]),
  tokenLimit: z.coerce
    .number()
    .int()
    .refine(
      (value) => value === -1 || value > 0,
      "share.validation.invalidTokenLimit",
    ),
  parallelLimit: z.coerce
    .number()
    .int()
    .refine(
      (value) => value === -1 || value >= 3,
      "share.validation.invalidParallelLimit",
    ),
  expiresInSecs: z.coerce.number().int().positive("share.validation.required"),
  apiKey: z
    .string()
    .trim()
    .optional()
    .transform((value) => value ?? "")
    .refine(
      (value) => value.length === 0 || /^[A-Za-z0-9._-]{8,128}$/.test(value),
      "share.validation.invalidApiKey",
    ),
  subdomain: z
    .string()
    .trim()
    .optional()
    .transform((value) => value ?? "")
    .refine(
      (value) =>
        value.length === 0 ||
        (/^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])?$/.test(value) &&
          !["admin", "api", "www", "cdn-cgi"].includes(value)),
      "share.validation.invalidSubdomain",
    ),
});

export const tunnelConfigSchema = z.object({
  domain: z.string().trim().min(1, "share.validation.required"),
});

export type CreateShareFormValues = z.infer<typeof createShareSchema>;
export type TunnelConfigFormValues = z.infer<typeof tunnelConfigSchema>;
export type CreateShareFormInput = z.input<typeof createShareSchema>;
