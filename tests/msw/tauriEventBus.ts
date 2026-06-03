// P9-D：vi.mock 的 `@tauri-apps/api/event::listen` 只对静态 import 生效，集成测试
// 走 `await import("@tauri-apps/api/event")` 拿 REAL 实现。REAL 实现内部调用：
//   1) `transformCallback(handler)` → 拿 callback id
//   2) `invoke("plugin:event|listen", { event, handler: <id> })` → 注册（返回 listener id）
//   3) emit 时由 Tauri runtime 调 `window[\`_\${id}\`](payload)`（jsdom 下不可达）
//
// 这个 bus 把 (transformCallback, listen invoke, emit) 串起来，让真模块 + 动态
// import 路径也能工作；同时也是 tauriMocks.ts 里 `emitTauriEvent` 的统一注册表。

type Callback = (...args: unknown[]) => unknown;

let nextCallbackId = 1;
const idToCallback = new Map<number, Callback>();

interface ListenerRecord {
  event: string;
  callbackId: number;
}
let nextListenerId = 1;
const listenerById = new Map<number, ListenerRecord>();
const listenersByEvent = new Map<string, Set<number>>();

const ensureEventSet = (event: string) => {
  if (!listenersByEvent.has(event)) listenersByEvent.set(event, new Set());
  return listenersByEvent.get(event)!;
};

/**
 * 真 transformCallback 的测试桩：把 handler 收进 id 注册表，返回 id。
 * 同时把 `window[\`_\${id}\`]` 设上方便 REAL runtime 兜底走 window lookup。
 */
export function registerCallback(callback: Callback): number {
  const id = nextCallbackId++;
  idToCallback.set(id, callback);
  if (typeof globalThis.window !== "undefined") {
    (globalThis.window as unknown as Record<string, unknown>)[`_${id}`] =
      callback;
  }
  return id;
}

/**
 * 给 vi.mock("@tauri-apps/api/event") 的静态 import 路径用的 listen 注册入口。
 * 直接接收 handler 函数；listener 表项里 `callbackId = 0` 表示"句柄式"。
 */
export function registerEventHandler(event: string, handler: Callback): () => void {
  const callbackId = registerCallback(handler);
  const listenerId = nextListenerId++;
  listenerById.set(listenerId, { event, callbackId });
  ensureEventSet(event).add(listenerId);
  return () => {
    listenerById.delete(listenerId);
    listenersByEvent.get(event)?.delete(listenerId);
  };
}

/**
 * 把测试 emit 派发给所有匹配 listener。两种 import 路径共享同一份注册表。
 */
export function emitEvent(event: string, payload: unknown) {
  const ids = listenersByEvent.get(event);
  if (!ids) return;
  for (const listenerId of ids) {
    const record = listenerById.get(listenerId);
    if (!record) continue;
    const cb = idToCallback.get(record.callbackId);
    cb?.({ event, id: listenerId, payload });
  }
}

/**
 * 在 setupGlobals 安装的 __TAURI_INTERNALS__.invoke 里调用。返回 undefined 表示
 * 本命令不归 event bus 管，调用方继续走 MSW 转发。
 */
export function invokeCommand(
  command: string,
  payload: Record<string, unknown>,
): unknown {
  if (command === "plugin:event|listen") {
    const event = String(payload.event ?? "");
    const callbackId = Number(payload.handler ?? 0);
    if (!event || !callbackId) return undefined;
    const listenerId = nextListenerId++;
    listenerById.set(listenerId, { event, callbackId });
    ensureEventSet(event).add(listenerId);
    return listenerId;
  }
  if (command === "plugin:event|unlisten") {
    const event = String(payload.event ?? "");
    const eventId = Number(payload.eventId ?? 0);
    if (!event || !eventId) return undefined;
    listenerById.delete(eventId);
    listenersByEvent.get(event)?.delete(eventId);
    return undefined;
  }
  return undefined;
}

/** 测试 teardown 用，避免上个 case 残留 handler 影响下一个。 */
export function resetEventBus() {
  nextCallbackId = 1;
  idToCallback.clear();
  nextListenerId = 1;
  listenerById.clear();
  listenersByEvent.clear();
}
