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
    const nativeResponsesPresets = new Map<
      string,
      { contextWindows: Record<string, number> }
    >([
      [
        "DouBaoSeed",
        { contextWindows: { "doubao-seed-2-1-pro-260628": 262144 } },
      ],
      ["Bailian", { contextWindows: { "qwen3-coder-plus": 1048576 } }],
      ["Longcat", { contextWindows: { "LongCat-2.0-Preview": 1048576 } }],
      ["MiniMax", { contextWindows: { "MiniMax-M3": 1000000 } }],
      ["MiniMax en", { contextWindows: { "MiniMax-M3": 1000000 } }],
      [
        "Xiaomi MiMo",
        {
          contextWindows: {
            "mimo-v2.5-pro": 1048576,
            "mimo-v2.5": 1048576,
          },
        },
      ],
      [
        "Xiaomi MiMo Token Plan (China)",
        {
          contextWindows: {
            "mimo-v2.5-pro": 1048576,
            "mimo-v2.5": 1048576,
          },
        },
      ],
    ]);

    for (const [name, expected] of nativeResponsesPresets) {
      const preset = codexProviderPresets.find((item) => item.name === name);

      expect(preset, `${name} preset`).toBeDefined();
      expect(preset?.apiFormat).toBe("openai_responses");
      // 原生 Responses 预设现在带 modelCatalog：cc-switch 直连时据此生成
      // ~/.codex 的 model-catalogs.json（shell_command 编辑、不发 freeform
      // apply_patch）。带 catalog 不再强制开“本地路由映射”——前端已按
      // apiFormat 解耦（openai_responses 默认不开接管）。
      expect((preset?.modelCatalog ?? []).length).toBeGreaterThan(0);
      expect(
        Object.fromEntries(
          (preset?.modelCatalog ?? []).map((model) => [
            model.model,
            model.contextWindow,
          ]),
        ),
      ).toEqual(expected.contextWindows);
      // 原生（直连）不走 Chat 转换，因此不需要 codexChatReasoning。
      expect(preset?.codexChatReasoning).toBeUndefined();
    }
  });
});
