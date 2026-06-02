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
// 测试里 tauriMocks 已经把 `@tauri-apps/api/core::invoke` 换成 MSW 路由，
// 但只有"看起来像 Tauri"时 invokeCommand 才会走 invoke；否则会 fallback 到
// `/web-api/invoke/...` 直接 fetch，jsdom 下解析成 localhost:3000 必失败。
// 这里假装 runtime 是 Tauri，让所有 IPC 都走得到 mock。
if (typeof globalThis.window !== "undefined") {
  (globalThis.window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ =
    {};
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
