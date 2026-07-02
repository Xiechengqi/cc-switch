// Icon metadata for search and categorization
import { IconMetadata } from "@/types/icon";

export const iconMetadata: Record<string, IconMetadata> = {
  aigocode: {
    name: "aigocode",
    displayName: "AIGoCode",
    category: "ai-provider",
    keywords: ["aigocode", "aigo", "code", "third-party"],
    defaultColor: "#5B7FFF",
  },
  apikeyfun: {
    name: "apikeyfun",
    displayName: "APIKEY.FUN",
    category: "ai-provider",
    keywords: [
      "apikeyfun",
      "api key",
      "gateway",
      "relay",
      "claude",
      "codex",
      "gemini",
    ],
    defaultColor: "#9C3F00",
  },
  apinebula: {
    name: "apinebula",
    displayName: "APINebula",
    category: "ai-provider",
    keywords: [
      "apinebula",
      "api nebula",
      "gateway",
      "relay",
      "claude",
      "codex",
      "gemini",
    ],
    defaultColor: "#C86F49",
  },
  atlascloud: {
    name: "atlascloud",
    displayName: "AtlasCloud",
    category: "ai-provider",
    keywords: [
      "atlascloud",
      "atlas cloud",
      "coding plan",
      "openai",
      "anthropic",
      "codex",
      "claude",
    ],
    defaultColor: "#111111",
  },
  sudocode: {
    name: "sudocode",
    displayName: "SudoCode",
    category: "ai-provider",
    keywords: [
      "sudocode",
      "sudo code",
      "gateway",
      "relay",
      "claude",
      "codex",
      "gemini",
      "openclaw",
    ],
    defaultColor: "#111111",
  },
  alibaba: {
    name: "alibaba",
    displayName: "Alibaba",
    category: "ai-provider",
    keywords: ["qwen", "tongyi"],
    defaultColor: "#FF6A00",
  },
};

export function getIconMetadata(name: string): IconMetadata | undefined {
  return iconMetadata[name.toLowerCase()];
}

export function searchIcons(query: string): string[] {
  const lowerQuery = query.toLowerCase();
  return Object.values(iconMetadata)
    .filter(
      (meta) =>
        meta.name.includes(lowerQuery) ||
        meta.displayName.toLowerCase().includes(lowerQuery) ||
        meta.keywords.some((k) => k.includes(lowerQuery)),
    )
    .map((meta) => meta.name);
}
