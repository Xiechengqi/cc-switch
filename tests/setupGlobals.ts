// Polyfill ResizeObserver for jsdom/happy-dom
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof globalThis.ResizeObserver;
}

// Radix Select / Dialog 在 jsdom 下要用这些 DOM 方法做焦点/指针管理。
// 缺失时 SelectTrigger 点击不响应（measuredWith 拿不到 capture 直接 throw）。
if (typeof Element !== "undefined") {
  if (typeof Element.prototype.hasPointerCapture === "undefined") {
    Element.prototype.hasPointerCapture = () => false;
  }
  if (typeof Element.prototype.releasePointerCapture === "undefined") {
    Element.prototype.releasePointerCapture = () => {};
  }
  if (typeof Element.prototype.setPointerCapture === "undefined") {
    Element.prototype.setPointerCapture = () => {};
  }
  if (typeof Element.prototype.scrollIntoView === "undefined") {
    Element.prototype.scrollIntoView = function () {};
  }
}

// `runtime.ts::isTauriRuntime` 看 `window.__TAURI_INTERNALS__` 决定 IPC 走法。
// 测试里 tauriMocks 用 vi.mock 把 `@tauri-apps/api/core::invoke` / `event::listen`
// 换成 MSW 路由——但只覆盖静态 import。集成测试里 `runtime.ts` / `useTauriEvent`
// 用 `await import("@tauri-apps/api/core")` 动态加载真实 Tauri 模块，绕过 vi.mock
// 拿到 REAL 实现；REAL 实现统一 proxy 到 `window.__TAURI_INTERNALS__.{invoke,
// transformCallback}`。所以这里在 globals 上挂一套完整 shim，让"真模块 + 动态
// import"路径也走得到 MSW / 测试 event emitter。
//
// 实现要点：
// - invoke：路由 `plugin:event|listen` / `unlisten` 到测试本地 listener 注册表，
//   其它命令转发到 MSW。
// - transformCallback：分配自增 id，把 handler 收进 `idToHandler`，返回 id；
//   真 `event::listen` 会把 id 传进 `plugin:event|listen` 让我们 wire 起来。
// - emitTauriEvent：把 (event, payload) 派发给所有匹配 listener。tauriMocks.ts
//   导出的同名 helper 共享同一份注册表（见 tests/msw/tauriEventBus.ts）。
import {
  invokeCommand as eventInvokeCommand,
  registerCallback,
} from "./msw/tauriEventBus";

const TAURI_ENDPOINT = "http://tauri.local";
const installTauriInternals = (target: Record<string, unknown>) => {
  target.__TAURI_INTERNALS__ = {
    transformCallback: (
      callback: (...args: unknown[]) => unknown,
      _once: boolean,
    ) => registerCallback(callback),
    invoke: async (
      command: string,
      payload: Record<string, unknown> = {},
    ) => {
      const routed = eventInvokeCommand(command, payload);
      if (routed !== undefined) return routed;
      const response = await fetch(`${TAURI_ENDPOINT}/${command}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
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
    // 多窗口 / WebviewWindow API。集成测试里 `App.tsx` 会调用
    // `getCurrentWebviewWindow().setDecorations`、`isMaximized` 等；不实现的话
    // 会抛"Cannot read properties of undefined"。给一个最小可用 stub 让 catch
    // 路径正常吞掉。
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  };
};
if (typeof globalThis.window !== "undefined") {
  installTauriInternals(globalThis.window as unknown as Record<string, unknown>);
}

const storage = new Map<string, string>();

if (
  typeof globalThis.localStorage === "undefined" ||
  typeof globalThis.localStorage?.getItem !== "function"
) {
  Object.defineProperty(globalThis, "localStorage", {
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => {
        storage.set(key, String(value));
      },
      removeItem: (key: string) => {
        storage.delete(key);
      },
      clear: () => {
        storage.clear();
      },
      key: (index: number) => Array.from(storage.keys())[index] ?? null,
      get length() {
        return storage.size;
      },
    },
    configurable: true,
  });
}

if (!Element.prototype.hasPointerCapture) {
  Element.prototype.hasPointerCapture = () => false;
}

if (!Element.prototype.setPointerCapture) {
  Element.prototype.setPointerCapture = () => {};
}

if (!Element.prototype.releasePointerCapture) {
  Element.prototype.releasePointerCapture = () => {};
}
