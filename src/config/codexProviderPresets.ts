/**
 * Codex 预设供应商配置模板
 */
import { ProviderCategory } from "../types";
import type {
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
} from "../types";
import type { PresetTheme } from "./claudeProviderPresets";

export interface CodexProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // 第三方供应商可提供单独的获取 API Key 链接
  apiKeyUrl?: string;
  auth: Record<string, any>; // 将写入 ~/.codex/auth.json
  config: string; // 将写入 ~/.codex/config.toml（TOML 字符串）
  isOfficial?: boolean; // 标识是否为官方预设
  isPartner?: boolean; // 标识是否为商业合作伙伴
  partnerPromotionKey?: string; // 合作伙伴促销信息的 i18n key
  category?: ProviderCategory; // 新增：分类
  isCustomTemplate?: boolean; // 标识是否为自定义模板
  // 新增：请求地址候选列表（用于地址管理/测速）
  endpointCandidates?: string[];
  // 新增：视觉主题配置
  theme?: PresetTheme;
  // 图标配置
  icon?: string; // 图标名称
  iconColor?: string; // 图标颜色
  // Codex API 格式
  apiFormat?: CodexApiFormat;
  // 特殊供应商类型
  providerType?:
    | "openai_device"
    | "openai_cli"
    | "openai_session"
    | "codex_oauth"
    | "cursor_oauth"
    | "cursor_apikey"
    | "openai_official_session";
  requiresOAuth?: boolean;
  // Codex Chat 本地路由模式下的模型目录
  modelCatalog?: CodexCatalogModel[];
  // Codex Responses -> Chat Completions reasoning capability defaults
  codexChatReasoning?: CodexChatReasoning;
}

/**
 * 生成第三方供应商的 auth.json
 */
export function generateThirdPartyAuth(apiKey: string): Record<string, any> {
  return {
    OPENAI_API_KEY: apiKey || "",
  };
}

/**
 * 生成第三方供应商的 config.toml
 */
export function generateThirdPartyConfig(
  providerName: string,
  baseUrl: string,
  modelName = "gpt-5.5",
): string {
  const tomlString = (value: string) => JSON.stringify(value);

  return `model_provider = "custom"
model = ${tomlString(modelName)}
model_reasoning_effort = "high"
disable_response_storage = true

[model_providers.custom]
name = ${tomlString(providerName)}
base_url = ${tomlString(baseUrl)}
wire_api = "responses"
requires_openai_auth = true`;
}

function modelCatalog(
  models: Array<
    string | { model: string; displayName?: string; contextWindow?: number }
  >,
): CodexCatalogModel[] {
  return models.map((entry) =>
    typeof entry === "string"
      ? { model: entry }
      : {
          model: entry.model,
          displayName: entry.displayName,
          contextWindow: entry.contextWindow,
        },
  );
}

export const codexProviderPresets: CodexProviderPreset[] = [
  {
    name: "openai device",
    websiteUrl: "https://chatgpt.com/codex",
    isOfficial: true,
    category: "official",
    auth: {},
    config: `model = "gpt-5.5"`,
    providerType: "openai_device",
    theme: {
      icon: "codex",
      backgroundColor: "#1F2937", // gray-800
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
  {
    name: "openai cli",
    websiteUrl: "https://chatgpt.com/codex",
    isOfficial: true,
    category: "official",
    auth: {},
    config: `model = "gpt-5.5"`,
    providerType: "openai_cli",
    theme: {
      icon: "codex",
      backgroundColor: "#111827",
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
  {
    name: "openai session",
    websiteUrl: "https://chatgpt.com/api/auth/session",
    isOfficial: true,
    category: "official",
    auth: {},
    config: `model = "gpt-5.5"`,
    providerType: "openai_session",
    theme: {
      icon: "codex",
      backgroundColor: "#111827",
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
  {
    name: "Cursor API Key",
    websiteUrl: "https://cursor.com/dashboard/cloud-agents",
    apiKeyUrl: "https://cursor.com/dashboard/cloud-agents",
    isOfficial: true,
    category: "official",
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "cursor",
      "https://api.cursor.com",
      "composer-2.5",
    ),
    providerType: "cursor_apikey",
    theme: {
      icon: "codex",
      backgroundColor: "#111111",
      textColor: "#FFFFFF",
    },
    icon: "cursor",
  },
  {
    name: "OpenRouter",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "openrouter",
      "https://openrouter.ai/api/v1",
      "gpt-5.4",
    ),
    category: "aggregator",
    icon: "openrouter",
    iconColor: "#6566F1",
  },
  {
    name: "Nvidia",
    websiteUrl: "https://build.nvidia.com",
    apiKeyUrl: "https://build.nvidia.com/settings/api-keys",
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "nvidia",
      "https://integrate.api.nvidia.com/v1",
      "moonshotai/kimi-k2.5",
    ),
    endpointCandidates: ["https://integrate.api.nvidia.com/v1"],
    apiFormat: "openai_chat",
    modelCatalog: modelCatalog([
      {
        model: "moonshotai/kimi-k2.5",
        displayName: "Kimi K2.5",
        contextWindow: 262144,
      },
    ]),
    codexChatReasoning: {
      supportsThinking: true,
      supportsEffort: false,
      thinkingParam: "thinking",
      effortParam: "none",
      outputFormat: "reasoning_content",
    },
    category: "aggregator",
    icon: "nvidia",
    iconColor: "#000000",
  },
  {
    name: "DeepSeek(API Key)",
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "deepseek",
      "https://api.deepseek.com",
      "deepseek-v4-flash",
    ),
    endpointCandidates: ["https://api.deepseek.com"],
    apiFormat: "openai_chat",
    modelCatalog: modelCatalog([
      {
        model: "deepseek-v4-flash",
        displayName: "DeepSeek V4 Flash",
        contextWindow: 1000000,
      },
      {
        model: "deepseek-v4-pro",
        displayName: "DeepSeek V4 Pro",
        contextWindow: 1000000,
      },
    ]),
    codexChatReasoning: {
      supportsThinking: true,
      supportsEffort: true,
      thinkingParam: "thinking",
      effortParam: "reasoning_effort",
      effortValueMode: "deepseek",
      outputFormat: "reasoning_content",
    },
    category: "cn_official",
    icon: "deepseek",
    iconColor: "#1E88E5",
  },
];
