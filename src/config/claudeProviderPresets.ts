/**
 * 预设供应商配置模板
 */
import { ProviderCategory } from "../types";

export interface TemplateValueConfig {
  label: string;
  placeholder: string;
  defaultValue?: string;
  editorValue: string;
}

/**
 * 预设供应商的视觉主题配置
 */
export interface PresetTheme {
  /** 图标类型：'claude' | 'codex' | 'gemini' | 'deepseek' | 'generic' */
  icon?: "claude" | "codex" | "gemini" | "deepseek" | "generic";
  /** 背景色（选中状态），支持 Tailwind 类名或 hex 颜色 */
  backgroundColor?: string;
  /** 文字色（选中状态），支持 Tailwind 类名或 hex 颜色 */
  textColor?: string;
}

export interface ProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // 新增：第三方/聚合等可单独配置获取 API Key 的链接
  apiKeyUrl?: string;
  settingsConfig: object;
  isOfficial?: boolean; // 标识是否为官方预设
  isPartner?: boolean; // 标识是否为商业合作伙伴
  partnerPromotionKey?: string; // 合作伙伴促销信息的 i18n key
  category?: ProviderCategory; // 新增：分类
  // 新增：指定该预设所使用的 API Key 字段名（默认 ANTHROPIC_AUTH_TOKEN）
  apiKeyField?: "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";
  // 新增：模板变量定义，用于动态替换配置中的值
  templateValues?: Record<string, TemplateValueConfig>; // editorValue 存储编辑器中的实时输入值
  // 新增：请求地址候选列表（用于地址管理/测速）
  endpointCandidates?: string[];
  // 新增：视觉主题配置
  theme?: PresetTheme;
  // 图标配置
  icon?: string; // 图标名称
  iconColor?: string; // 图标颜色

  // Claude API 格式（仅 Claude 供应商使用）
  // - "anthropic" (默认): Anthropic Messages API 格式，直接透传
  // - "openai_chat": OpenAI Chat Completions 格式，需要格式转换
  // - "openai_responses": OpenAI Responses API 格式，需要格式转换
  // - "gemini_native": Gemini Native generateContent API 格式，需要格式转换
  apiFormat?:
    | "anthropic"
    | "openai_chat"
    | "openai_responses"
    | "gemini_native";

  // 供应商类型标识（用于特殊供应商检测）
  // - "github_copilot": GitHub Copilot 供应商（需要 OAuth 认证）
  // - "codex_oauth": OpenAI Codex via ChatGPT Plus/Pro 反代（需要 OAuth 认证）
  // - "claude_oauth": Claude 官方订阅 OAuth（Anthropic 官方）
  // - "deepseek_account": DeepSeek 账号
  providerType?:
    | "github_copilot"
    | "codex_oauth"
    | "claude_oauth"
    | "google_gemini_oauth"
    | "deepseek_account";

  // 是否需要 OAuth 认证（而非 API Key）
  requiresOAuth?: boolean;

  // 是否在 UI 中隐藏该预设（预设仍存在，仅不在列表中显示）
  hidden?: boolean;

  // 获取模型列表使用的完整 URL（覆写自动候选逻辑）
  // 缺省时后端基于 baseURL 自动尝试 /v1/models、/models 以及剥离已知兼容子路径后的变体。
  modelsUrl?: string;
}

export const providerPresets: ProviderPreset[] = [
  {
    name: "Claude Official",
    websiteUrl: "https://www.anthropic.com/claude-code",
    settingsConfig: {
      env: {},
    },
    isOfficial: true, // 明确标识为官方预设
    category: "official",
    providerType: "claude_oauth",
    requiresOAuth: true,
    theme: {
      icon: "claude",
      backgroundColor: "#D97757",
      textColor: "#FFFFFF",
    },
    icon: "anthropic",
    iconColor: "#D4915D",
  },
  {
    name: "OpenAI Official",
    websiteUrl: "https://chatgpt.com/codex",
    settingsConfig: {
      env: {
        // base_url 由代理后端强制重写为 chatgpt.com/backend-api/codex
        // 用户无需配置
        ANTHROPIC_BASE_URL: "https://chatgpt.com/backend-api/codex",
        ANTHROPIC_MODEL: "gpt-5.5",
        ANTHROPIC_DEFAULT_HAIKU_MODEL: "gpt-5.4",
        ANTHROPIC_DEFAULT_SONNET_MODEL: "gpt-5.5",
        ANTHROPIC_DEFAULT_OPUS_MODEL: "gpt-5.5",
      },
    },
    isOfficial: true,
    category: "official",
    apiFormat: "openai_responses",
    providerType: "codex_oauth",
    requiresOAuth: true,
    theme: {
      icon: "codex",
      backgroundColor: "#1F2937",
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
  {
    name: "DeepSeek Official",
    websiteUrl: "https://chat.deepseek.com",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://chat.deepseek.com",
        ANTHROPIC_MODEL: "deepseek-v4-flash",
        ANTHROPIC_DEFAULT_HAIKU_MODEL: "deepseek-v4-flash",
        ANTHROPIC_DEFAULT_SONNET_MODEL: "deepseek-v4-flash",
        ANTHROPIC_DEFAULT_OPUS_MODEL: "deepseek-v4-pro",
      },
    },
    category: "cn_official",
    providerType: "deepseek_account",
    requiresOAuth: true,
    theme: {
      icon: "deepseek",
      backgroundColor: "#4D6BFE",
      textColor: "#FFFFFF",
    },
    icon: "deepseek",
    iconColor: "#4D6BFE",
  },
  {
    name: "GitHub Copilot",
    websiteUrl: "https://github.com/features/copilot",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://api.githubcopilot.com",
        ANTHROPIC_MODEL: "claude-sonnet-4.6",
        ANTHROPIC_DEFAULT_HAIKU_MODEL: "claude-haiku-4.5",
        ANTHROPIC_DEFAULT_SONNET_MODEL: "claude-sonnet-4.6",
        ANTHROPIC_DEFAULT_OPUS_MODEL: "claude-sonnet-4.6",
      },
    },
    category: "third_party",
    apiFormat: "openai_chat",
    providerType: "github_copilot",
    requiresOAuth: true,
    icon: "github",
    iconColor: "#000000",
  },
];
