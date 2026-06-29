import { describe, expect, it } from "vitest";
import { codexProviderPresets } from "@/config/codexProviderPresets";
import {
  extractCodexBaseUrl,
  extractCodexModelName,
  extractCodexWireApi,
} from "@/utils/providerConfigUtils";

const expectedChatPresets = new Map<
  string,
  { baseUrl: string; contextWindows: Record<string, number> }
>([
  [
    "Nvidia",
    {
      baseUrl: "https://integrate.api.nvidia.com/v1",
      contextWindows: { "moonshotai/kimi-k2.5": 262144 },
    },
  ],
  [
    "DeepSeek(API Key)",
    {
      baseUrl: "https://api.deepseek.com",
      contextWindows: {
        "deepseek-v4-flash": 1000000,
        "deepseek-v4-pro": 1000000,
      },
    },
  ],
]);

describe("Codex Chat provider presets", () => {
  it("marks migrated Chat Completions presets for local routing", () => {
    for (const [name, expected] of expectedChatPresets) {
      const preset = codexProviderPresets.find((item) => item.name === name);

      expect(preset, `${name} preset`).toBeDefined();
      expect(preset?.apiFormat).toBe("openai_chat");
      expect(extractCodexBaseUrl(preset?.config)).toBe(expected.baseUrl);
      expect(extractCodexWireApi(preset?.config)).toBe("responses");
      expect(preset?.endpointCandidates).toContain(expected.baseUrl);
      expect(preset?.modelCatalog?.length).toBeGreaterThan(0);
      expect(extractCodexModelName(preset?.config)).toBe(
        preset?.modelCatalog?.[0]?.model,
      );
      expect(
        Object.fromEntries(
          (preset?.modelCatalog ?? []).map((model) => [
            model.model,
            model.contextWindow,
          ]),
        ),
      ).toEqual(expected.contextWindows);
    }
  });

  it("uses native Responses API for migrated CN providers without local route mapping", () => {
    const nativeResponsesPresets = [
      "DouBaoSeed",
      "Bailian",
      "Longcat",
      "MiniMax",
      "MiniMax en",
      "Xiaomi MiMo",
      "Xiaomi MiMo Token Plan (China)",
    ];

    for (const name of nativeResponsesPresets) {
      const preset = codexProviderPresets.find((item) => item.name === name);

      expect(preset, `${name} preset`).toBeDefined();
      expect(preset?.apiFormat).toBe("openai_responses");
      // 原生 Responses 预设必须不带 modelCatalog，否则“本地路由映射”开关会默认勾选
      expect(preset?.modelCatalog ?? []).toHaveLength(0);
      expect(preset?.codexChatReasoning).toBeUndefined();
    }
  });
});
