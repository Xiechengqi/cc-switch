import "cross-fetch/polyfill";
import { vi } from "vitest";
import { server } from "./server";
import {
  emitEvent,
  invokeCommand as eventInvokeCommand,
  registerEventHandler,
} from "./tauriEventBus";

const TAURI_ENDPOINT = "http://tauri.local";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (command: string, payload: Record<string, unknown> = {}) => {
    // 与 setupGlobals 里 __TAURI_INTERNALS__.invoke 共享 event bus，保证静态 import
    // 和动态 import 路径都能 listen / unlisten。
    const routed = eventInvokeCommand(command, payload);
    if (routed !== undefined) return routed;
    const response = await fetch(`${TAURI_ENDPOINT}/${command}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify(payload ?? {}),
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(text || `Invoke failed for ${command}`);
    }

    const text = await response.text();
    if (!text) return undefined;
    try {
      return JSON.parse(text);
    } catch {
      return text;
    }
  },
  transformCallback: (
    callback: (...args: unknown[]) => unknown,
    _once: boolean,
  ) => {
    // 复用 setupGlobals 的 registerCallback：保持 callback id 命名空间统一。
    const internals = (globalThis.window as unknown as {
      __TAURI_INTERNALS__: {
        transformCallback: (
          cb: (...args: unknown[]) => unknown,
          once: boolean,
        ) => number;
      };
    }).__TAURI_INTERNALS__;
    return internals.transformCallback(callback, _once);
  },
}));

/**
 * 测试触发 Tauri 事件：把 (event, payload) 派发给所有已注册 listener。
 * 不论 listener 通过静态 `@tauri-apps/api/event::listen` 还是动态 import 的 REAL
 * listen 注册，都共享同一份 tauriEventBus 注册表。
 */
export const emitTauriEvent = (event: string, payload: unknown) => {
  emitEvent(event, payload);
};

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (
    event: string,
    handler: (event: { payload: unknown }) => void,
  ) => {
    return registerEventHandler(event, handler as never);
  },
}));

// Ensure the MSW server is referenced so tree shaking doesn't remove imports
void server;

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: async () => "/home/mock",
  join: async (...segments: string[]) => segments.join("/"),
}));
