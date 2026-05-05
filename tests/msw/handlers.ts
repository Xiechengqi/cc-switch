import { http, HttpResponse } from "msw";
import type { AppId } from "@/lib/api/types";
import type { McpServer, Provider, Settings } from "@/types";
import {
  addProvider,
  addShare,
  deleteProvider,
  deleteSession,
  getEmailAuthSession,
  getEmailAuthStatus,
  getShare,
  getShareConnectInfo,
  getTunnelStatus,
  getCurrentProviderId,
  getLiveProviderIds,
  getSessionMessages,
  getProviders,
  listProviders,
  listShares,
  listSessions,
  removeShare,
  resetProviderState,
  setCurrentProviderId,
  setTunnelStatus,
  updateProvider,
  updateShare,
  updateSortOrder,
  getSettings,
  setSettings,
  getAppConfigDirOverride,
  setAppConfigDirOverrideState,
  setEmailAuthSession,
  setEmailAuthStatus,
  getMcpConfig,
  setMcpServerEnabled,
  upsertMcpServer,
  deleteMcpServer,
} from "./state";

const TAURI_ENDPOINT = "http://tauri.local";

const withJson = async <T>(request: Request): Promise<T> => {
  try {
    const body = await request.text();
    if (!body) return {} as T;
    return JSON.parse(body) as T;
  } catch {
    return {} as T;
  }
};

const success = <T>(payload: T) => HttpResponse.json(payload as any);

export const handlers = [
  http.post(`${TAURI_ENDPOINT}/get_migration_result`, () => success(false)),
  http.post(`${TAURI_ENDPOINT}/get_skills_migration_result`, () =>
    success(null),
  ),
  http.post(`${TAURI_ENDPOINT}/get_providers`, async ({ request }) => {
    const { app } = await withJson<{ app: AppId }>(request);
    return success(getProviders(app));
  }),

  http.post(`${TAURI_ENDPOINT}/get_current_provider`, async ({ request }) => {
    const { app } = await withJson<{ app: AppId }>(request);
    return success(getCurrentProviderId(app));
  }),

  http.post(
    `${TAURI_ENDPOINT}/update_providers_sort_order`,
    async ({ request }) => {
      const { updates = [], app } = await withJson<{
        updates: { id: string; sortIndex: number }[];
        app: AppId;
      }>(request);
      updateSortOrder(app, updates);
      return success(true);
    },
  ),

  http.post(`${TAURI_ENDPOINT}/update_tray_menu`, () => success(true)),

  http.post(`${TAURI_ENDPOINT}/get_opencode_live_provider_ids`, () =>
    success(getLiveProviderIds("opencode")),
  ),

  http.post(`${TAURI_ENDPOINT}/get_openclaw_live_provider_ids`, () =>
    success(getLiveProviderIds("openclaw")),
  ),

  http.post(`${TAURI_ENDPOINT}/get_openclaw_default_model`, () =>
    success({ primary: null, fallback: [] }),
  ),

  http.post(`${TAURI_ENDPOINT}/scan_openclaw_config_health`, () => success([])),

  http.post(`${TAURI_ENDPOINT}/switch_provider`, async ({ request }) => {
    const { id, app } = await withJson<{ id: string; app: AppId }>(request);
    const providers = listProviders(app);
    if (!providers[id]) {
      return HttpResponse.json(false, { status: 404 });
    }
    setCurrentProviderId(app, id);
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/add_provider`, async ({ request }) => {
    const { provider, app } = await withJson<{
      provider: Provider & { id?: string };
      app: AppId;
    }>(request);

    const newId = provider.id ?? `mock-${Date.now()}`;
    addProvider(app, { ...provider, id: newId });
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/update_provider`, async ({ request }) => {
    const { provider, app } = await withJson<{
      provider: Provider;
      app: AppId;
    }>(request);
    updateProvider(app, provider);
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/delete_provider`, async ({ request }) => {
    const { id, app } = await withJson<{ id: string; app: AppId }>(request);
    deleteProvider(app, id);
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/import_default_config`, async () => {
    resetProviderState();
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/open_external`, () => success(true)),

  http.post(`${TAURI_ENDPOINT}/list_sessions`, () => success(listSessions())),

  http.post(`${TAURI_ENDPOINT}/get_session_messages`, async ({ request }) => {
    const { providerId, sourcePath } = await withJson<{
      providerId: string;
      sourcePath: string;
    }>(request);
    return success(getSessionMessages(providerId, sourcePath));
  }),

  http.post(`${TAURI_ENDPOINT}/delete_session`, async ({ request }) => {
    const { providerId, sessionId, sourcePath } = await withJson<{
      providerId: string;
      sessionId: string;
      sourcePath: string;
    }>(request);
    return success(deleteSession(providerId, sessionId, sourcePath));
  }),

  http.post(`${TAURI_ENDPOINT}/delete_sessions`, async ({ request }) => {
    const { items = [] } = await withJson<{
      items?: {
        providerId: string;
        sessionId: string;
        sourcePath: string;
      }[];
    }>(request);

    return success(
      items.map((item) => ({
        providerId: item.providerId,
        sessionId: item.sessionId,
        sourcePath: item.sourcePath,
        success: deleteSession(
          item.providerId,
          item.sessionId,
          item.sourcePath,
        ),
      })),
    );
  }),

  // MCP APIs
  http.post(`${TAURI_ENDPOINT}/get_mcp_config`, async ({ request }) => {
    const { app } = await withJson<{ app: AppId }>(request);
    return success(getMcpConfig(app));
  }),

  http.post(`${TAURI_ENDPOINT}/import_mcp_from_claude`, () => success(1)),
  http.post(`${TAURI_ENDPOINT}/import_mcp_from_codex`, () => success(1)),

  http.post(`${TAURI_ENDPOINT}/set_mcp_enabled`, async ({ request }) => {
    const { app, id, enabled } = await withJson<{
      app: AppId;
      id: string;
      enabled: boolean;
    }>(request);
    setMcpServerEnabled(app, id, enabled);
    return success(true);
  }),

  http.post(
    `${TAURI_ENDPOINT}/upsert_mcp_server_in_config`,
    async ({ request }) => {
      const { app, id, spec } = await withJson<{
        app: AppId;
        id: string;
        spec: McpServer;
      }>(request);
      upsertMcpServer(app, id, spec);
      return success(true);
    },
  ),

  http.post(
    `${TAURI_ENDPOINT}/delete_mcp_server_in_config`,
    async ({ request }) => {
      const { app, id } = await withJson<{ app: AppId; id: string }>(request);
      deleteMcpServer(app, id);
      return success(true);
    },
  ),

  http.post(`${TAURI_ENDPOINT}/restart_app`, () => success(true)),

  http.post(`${TAURI_ENDPOINT}/get_settings`, () => success(getSettings())),

  http.post(`${TAURI_ENDPOINT}/check_env_conflicts`, () => success([])),

  http.post(`${TAURI_ENDPOINT}/save_settings`, async ({ request }) => {
    const { settings } = await withJson<{ settings: Settings }>(request);
    setSettings(settings);
    return success(true);
  }),

  http.post(`${TAURI_ENDPOINT}/list_shares`, () => success(listShares())),

  http.post(`${TAURI_ENDPOINT}/get_share_detail`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    return success(getShare(shareId));
  }),

  http.post(`${TAURI_ENDPOINT}/email_auth_get_status`, () =>
    success(getEmailAuthStatus()),
  ),

  http.post(`${TAURI_ENDPOINT}/email_auth_session_me`, () =>
    success(getEmailAuthSession()),
  ),

  http.post(
    `${TAURI_ENDPOINT}/email_auth_request_code`,
    async ({ request }) => {
      const { email } = await withJson<{ email: string }>(request);
      return success({
        ok: true,
        cooldownSecs: 60,
        maskedDestination: email || "***",
      });
    },
  ),

  http.post(`${TAURI_ENDPOINT}/email_auth_verify_code`, async ({ request }) => {
    const { email } = await withJson<{ email: string }>(request);
    const status = {
      authenticated: true,
      email: email || "owner@example.com",
      expiresAt: Date.now() / 1000 + 3600,
    };
    setEmailAuthStatus(status);
    setEmailAuthSession({
      authenticated: true,
      user: {
        id: "email-user-1",
        email: status.email ?? "owner@example.com",
      },
      expiresAt: new Date(Date.now() + 3600 * 1000).toISOString(),
    });
    return success(status);
  }),

  http.post(
    `${TAURI_ENDPOINT}/email_auth_request_owner_change_code`,
    async ({ request }) => {
      const { newEmail } = await withJson<{ newEmail: string }>(request);
      return success({
        ok: true,
        cooldownSecs: 60,
        maskedDestination: newEmail || "***",
      });
    },
  ),

  http.post(
    `${TAURI_ENDPOINT}/email_auth_change_owner_email`,
    async ({ request }) => {
      const { currentEmail, newEmail } = await withJson<{
        currentEmail: string;
        newEmail: string;
      }>(request);
      listShares()
        .filter((share) => share.ownerEmail === currentEmail)
        .forEach((share) =>
          updateShare(share.id, {
            name: newEmail,
            ownerEmail: newEmail,
          }),
        );
      const status = {
        authenticated: true,
        email: newEmail || "new-owner@example.com",
        expiresAt: Date.now() / 1000 + 3600,
      };
      setEmailAuthStatus(status);
      setEmailAuthSession({
        authenticated: true,
        user: {
          id: "email-user-2",
          email: status.email ?? "new-owner@example.com",
        },
        expiresAt: new Date(Date.now() + 3600 * 1000).toISOString(),
        installationOwnerEmail: status.email,
      });
      return success(status);
    },
  ),

  http.post(`${TAURI_ENDPOINT}/create_share`, async ({ request }) => {
    const { params } = await withJson<{
      params: {
        description?: string;
        forSale: "Yes" | "No" | "Free";
        tokenLimit: number;
        parallelLimit: number;
        expiresInSecs: number;
      };
    }>(request);

    if (listShares().length > 0) {
      return HttpResponse.json(
        "当前版本的分享能力基于本地代理服务，一个 cc-switch 只能创建一个分享",
        { status: 400 },
      );
    }

    const now = Date.now();
    const share = {
      id: `share-${now}`,
      name: "owner@example.com",
      ownerEmail: "owner@example.com",
      sharedWithEmails: [],
      description: params.description ?? null,
      forSale: params.forSale ?? "No",
      shareToken: `token-${now}`,
      appType: "proxy",
      providerId: null,
      apiKey: "",
      settingsConfig: null,
      tokenLimit: params.tokenLimit,
      parallelLimit: params.parallelLimit,
      tokensUsed: 0,
      requestsCount: 0,
      expiresAt: new Date(now + params.expiresInSecs * 1000).toISOString(),
      subdomain: null,
      tunnelUrl: null,
      status: "active",
      createdAt: new Date(now).toISOString(),
      lastUsedAt: null,
    };

    addShare(share as any);
    return success(share);
  }),

  http.post(`${TAURI_ENDPOINT}/delete_share`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    removeShare(shareId);
    return success(null);
  }),

  http.post(`${TAURI_ENDPOINT}/pause_share`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    updateShare(shareId, { status: "paused" });
    return success(null);
  }),

  http.post(`${TAURI_ENDPOINT}/resume_share`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    updateShare(shareId, { status: "active" });
    return success(null);
  }),

  http.post(
    `${TAURI_ENDPOINT}/update_share_description`,
    async ({ request }) => {
      const { params } = await withJson<{
        params: { shareId: string; description?: string };
      }>(request);
      updateShare(params.shareId, {
        description: params.description?.trim() || null,
      });
      return success(getShare(params.shareId));
    },
  ),

  http.post(
    `${TAURI_ENDPOINT}/update_share_parallel_limit`,
    async ({ request }) => {
      const { params } = await withJson<{
        params: { shareId: string; parallelLimit: number };
      }>(request);
      updateShare(params.shareId, { parallelLimit: params.parallelLimit });
      return success(getShare(params.shareId));
    },
  ),

  http.post(`${TAURI_ENDPOINT}/update_share_for_sale`, async ({ request }) => {
    const { params } = await withJson<{
      params: { shareId: string; forSale: "Yes" | "No" };
    }>(request);
    updateShare(params.shareId, { forSale: params.forSale });
    return success(getShare(params.shareId));
  }),

  http.post(`${TAURI_ENDPOINT}/start_share_tunnel`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    const share = getShare(shareId);
    if (!share) {
      return HttpResponse.json("Share not found", { status: 404 });
    }

    const info = {
      tunnelUrl: `https://${shareId}.example.test`,
      subdomain: `${shareId}-sub`,
      remotePort: 443,
      healthy: true,
    };

    setTunnelStatus(shareId, info);
    updateShare(shareId, {
      tunnelUrl: info.tunnelUrl,
      subdomain: info.subdomain,
    });

    return success(info);
  }),

  http.post(`${TAURI_ENDPOINT}/enable_share`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    const share = getShare(shareId);
    if (!share) {
      return HttpResponse.json("Share not found", { status: 404 });
    }

    const info = {
      tunnelUrl: `https://${shareId}.example.test`,
      subdomain: share.subdomain ?? `${shareId}-sub`,
      remotePort: 443,
      healthy: true,
    };

    setTunnelStatus(shareId, info);
    updateShare(shareId, {
      status: "active",
      tunnelUrl: info.tunnelUrl,
      subdomain: info.subdomain,
    });

    return success(info);
  }),

  http.post(`${TAURI_ENDPOINT}/disable_share`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    setTunnelStatus(shareId, null);
    updateShare(shareId, { status: "paused", tunnelUrl: null });
    return success(null);
  }),

  http.post(`${TAURI_ENDPOINT}/stop_share_tunnel`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    setTunnelStatus(shareId, null);
    return success(null);
  }),

  http.post(`${TAURI_ENDPOINT}/get_tunnel_status`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    return success(getTunnelStatus(shareId));
  }),

  http.post(`${TAURI_ENDPOINT}/get_share_connect_info`, async ({ request }) => {
    const { shareId } = await withJson<{ shareId: string }>(request);
    const info = getShareConnectInfo(shareId);
    if (!info) {
      return HttpResponse.json("Share not found", { status: 404 });
    }
    return success(info);
  }),

  http.post(`${TAURI_ENDPOINT}/get_request_logs`, () =>
    success({
      data: [],
      total: 0,
      page: 0,
      pageSize: 10,
    }),
  ),

  http.post(`${TAURI_ENDPOINT}/configure_tunnel`, async ({ request }) => {
    const { config } = await withJson<{
      config: {
        domain: string;
      };
    }>(request);
    setSettings({
      shareRouterDomain: config.domain,
    });
    return success(null);
  }),

  http.post(
    `${TAURI_ENDPOINT}/set_app_config_dir_override`,
    async ({ request }) => {
      const { path } = await withJson<{ path: string | null }>(request);
      setAppConfigDirOverrideState(path ?? null);
      return success(true);
    },
  ),

  http.post(`${TAURI_ENDPOINT}/get_app_config_dir_override`, () =>
    success(getAppConfigDirOverride()),
  ),

  http.post(
    `${TAURI_ENDPOINT}/apply_claude_plugin_config`,
    async ({ request }) => {
      const { official } = await withJson<{ official: boolean }>(request);
      setSettings({ enableClaudePluginIntegration: !official });
      return success(true);
    },
  ),

  http.post(`${TAURI_ENDPOINT}/apply_claude_onboarding_skip`, () =>
    success(true),
  ),

  http.post(`${TAURI_ENDPOINT}/clear_claude_onboarding_skip`, () =>
    success(true),
  ),

  http.post(`${TAURI_ENDPOINT}/get_config_dir`, async ({ request }) => {
    const { app } = await withJson<{ app: AppId }>(request);
    return success(app === "claude" ? "/default/claude" : "/default/codex");
  }),

  http.post(`${TAURI_ENDPOINT}/is_portable_mode`, () => success(false)),

  http.post(
    `${TAURI_ENDPOINT}/select_config_directory`,
    async ({ request }) => {
      const { defaultPath, default_path } = await withJson<{
        defaultPath?: string;
        default_path?: string;
      }>(request);
      const initial = defaultPath ?? default_path;
      return success(initial ? `${initial}/picked` : "/mock/selected-dir");
    },
  ),

  http.post(`${TAURI_ENDPOINT}/pick_directory`, async ({ request }) => {
    const { defaultPath, default_path } = await withJson<{
      defaultPath?: string;
      default_path?: string;
    }>(request);
    const initial = defaultPath ?? default_path;
    return success(initial ? `${initial}/picked` : "/mock/selected-dir");
  }),

  http.post(`${TAURI_ENDPOINT}/open_file_dialog`, () =>
    success("/mock/import-settings.json"),
  ),

  http.post(
    `${TAURI_ENDPOINT}/import_config_from_file`,
    async ({ request }) => {
      const { filePath } = await withJson<{ filePath: string }>(request);
      if (!filePath) {
        return success({ success: false, message: "Missing file" });
      }
      setSettings({ language: "en" });
      return success({ success: true, backupId: "backup-123" });
    },
  ),

  http.post(`${TAURI_ENDPOINT}/export_config_to_file`, async ({ request }) => {
    const { filePath } = await withJson<{ filePath: string }>(request);
    if (!filePath) {
      return success({ success: false, message: "Invalid destination" });
    }
    return success({ success: true, filePath });
  }),

  http.post(`${TAURI_ENDPOINT}/save_file_dialog`, () =>
    success("/mock/export-settings.json"),
  ),

  // Sync current providers live (no-op success)
  http.post(`${TAURI_ENDPOINT}/sync_current_providers_live`, () =>
    success({ success: true }),
  ),

  // Proxy status (for SettingsPage / ProxyPanel hooks)
  http.post(`${TAURI_ENDPOINT}/get_proxy_status`, () =>
    success({
      running: false,
      address: "127.0.0.1",
      port: 0,
      active_connections: 0,
      total_requests: 0,
      success_requests: 0,
      failed_requests: 0,
      success_rate: 0,
      uptime_seconds: 0,
      current_provider: null,
      current_provider_id: null,
      last_request_at: null,
      last_error: null,
      failover_count: 0,
      active_targets: [],
    }),
  ),

  http.post(`${TAURI_ENDPOINT}/get_proxy_takeover_status`, () =>
    success({
      claude: false,
      codex: false,
      gemini: false,
    }),
  ),

  http.post(`${TAURI_ENDPOINT}/start_proxy_server`, () =>
    success({
      address: "127.0.0.1",
      port: 53000,
    }),
  ),

  http.post(`${TAURI_ENDPOINT}/set_proxy_takeover_for_app`, () =>
    success(true),
  ),

  http.post(`${TAURI_ENDPOINT}/is_live_takeover_active`, () => success(false)),

  // Failover / circuit breaker defaults
  http.post(`${TAURI_ENDPOINT}/get_failover_queue`, () => success([])),
  http.post(`${TAURI_ENDPOINT}/get_available_providers_for_failover`, () =>
    success([]),
  ),
  http.post(`${TAURI_ENDPOINT}/add_to_failover_queue`, () => success(true)),
  http.post(`${TAURI_ENDPOINT}/remove_from_failover_queue`, () =>
    success(true),
  ),
  http.post(`${TAURI_ENDPOINT}/reorder_failover_queue`, () => success(true)),
  http.post(`${TAURI_ENDPOINT}/set_failover_item_enabled`, () => success(true)),

  http.post(`${TAURI_ENDPOINT}/get_circuit_breaker_config`, () =>
    success({
      failureThreshold: 3,
      successThreshold: 2,
      timeoutSeconds: 60,
      errorRateThreshold: 50,
      minRequests: 5,
    }),
  ),
  http.post(`${TAURI_ENDPOINT}/update_circuit_breaker_config`, () =>
    success(true),
  ),
  http.post(`${TAURI_ENDPOINT}/get_provider_health`, () =>
    success({
      provider_id: "mock-provider",
      app_type: "claude",
      is_healthy: true,
      consecutive_failures: 0,
      last_success_at: null,
      last_failure_at: null,
      last_error: null,
      updated_at: new Date().toISOString(),
    }),
  ),
  http.post(`${TAURI_ENDPOINT}/reset_circuit_breaker`, () => success(true)),
  http.post(`${TAURI_ENDPOINT}/get_circuit_breaker_stats`, () => success(null)),
];
