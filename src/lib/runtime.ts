export type RuntimeMode = "tauri" | "web";

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function runtimeMode(): RuntimeMode {
  return isTauriRuntime() ? "tauri" : "web";
}

export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (isTauriRuntime()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(command, args);
  }

  const response = await fetch(`/web-api/invoke/${encodeURIComponent(command)}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(args ?? {}),
  });

  if (!response.ok) {
    const text = await response.text();
    try {
      const parsed = JSON.parse(text);
      if (parsed?.error) {
        throw new Error(parsed.error);
      }
    } catch {
      // Fall through to the raw response body below when it is not JSON.
    }
    throw new Error(text || `HTTP ${response.status}`);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json() as Promise<T>;
}

export interface WebRuntimeContext {
  mode: "local-admin" | "share";
  shareId?: string;
  shareName?: string;
  subdomain?: string | null;
  appType?: string;
  status?: string;
  permissions?: string[];
}

export async function getWebRuntimeContext(): Promise<WebRuntimeContext> {
  if (isTauriRuntime()) {
    return { mode: "local-admin" };
  }
  const response = await fetch("/web-api/context", {
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(await response.text());
  }
  return response.json() as Promise<WebRuntimeContext>;
}
