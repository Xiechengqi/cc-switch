const AUTH_KEY = "cc_switch_router_auth_v1";

export interface RouterAuthState {
  installationId?: string | null;
  publicKey?: string | null;
  privateKey?: string | null;
  email?: string | null;
  accessToken?: string | null;
  refreshToken?: string | null;
  expiresAt?: string | null;
  refreshExpiresAt?: string | null;
}

export interface RouterSessionStatus {
  authenticated: boolean;
  user?: {
    id: string;
    email: string;
  } | null;
  expiresAt?: string | null;
  installationOwnerEmail?: string | null;
  isAdmin?: boolean;
}

function readAuthState(): RouterAuthState {
  try {
    return JSON.parse(localStorage.getItem(AUTH_KEY) || "{}") || {};
  } catch {
    return {};
  }
}

function writeAuthState(state: RouterAuthState): void {
  localStorage.setItem(AUTH_KEY, JSON.stringify(state));
}

function mergeAuthState(patch: RouterAuthState): RouterAuthState {
  const next = { ...readAuthState(), ...patch };
  writeAuthState(next);
  window.dispatchEvent(
    new CustomEvent("router-auth-changed", { detail: next }),
  );
  return next;
}

export function clearRouterSessionTokens(): void {
  const state = readAuthState();
  mergeAuthState({
    installationId: state.installationId || null,
    publicKey: state.publicKey || null,
    privateKey: state.privateKey || null,
    email: null,
    accessToken: null,
    refreshToken: null,
    expiresAt: null,
    refreshExpiresAt: null,
  });
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary);
}

function base64ToBytes(value: string): Uint8Array {
  return Uint8Array.from(atob(value), (ch) => ch.charCodeAt(0));
}

function bytesToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(
    bytes.byteOffset,
    bytes.byteOffset + bytes.byteLength,
  ) as ArrayBuffer;
}

function platformLabel(): string {
  const ua = navigator.userAgent || "";
  if (/Mac/i.test(ua)) return "web-macos";
  if (/Windows/i.test(ua)) return "web-windows";
  if (/Linux/i.test(ua)) return "web-linux";
  return "web";
}

function randomId(): string {
  return crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random()}`;
}

async function parseJsonResponse<T>(response: Response): Promise<T> {
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(data?.message || data?.error || `HTTP ${response.status}`);
  }
  return data as T;
}

async function generateInstallationKeys(): Promise<{
  publicKey: string;
  privateKey: string;
}> {
  const keyPair = (await crypto.subtle.generateKey(
    { name: "Ed25519" } as AlgorithmIdentifier,
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const publicKey = bytesToBase64(
    new Uint8Array(await crypto.subtle.exportKey("raw", keyPair.publicKey)),
  );
  const privateKey = bytesToBase64(
    new Uint8Array(await crypto.subtle.exportKey("pkcs8", keyPair.privateKey)),
  );
  return { publicKey, privateKey };
}

async function importPrivateKey(privateKeyBase64: string): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "pkcs8",
    bytesToArrayBuffer(base64ToBytes(privateKeyBase64)),
    { name: "Ed25519" } as AlgorithmIdentifier,
    false,
    ["sign"],
  );
}

async function registerInstallationIdentity(
  publicKey: string,
): Promise<string> {
  const response = await fetch("/v1/installations/register", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      publicKey,
      platform: platformLabel(),
      appVersion: "cc-switch-share-web",
      instanceNonce: randomId(),
    }),
  });
  const data = await parseJsonResponse<{ installationId: string }>(response);
  return data.installationId;
}

async function ensureInstallationIdentity(): Promise<{
  installationId: string;
  publicKey: string;
  privateKey: string;
}> {
  const state = readAuthState();
  if (state.installationId && state.publicKey && state.privateKey) {
    return {
      installationId: state.installationId,
      publicKey: state.publicKey,
      privateKey: state.privateKey,
    };
  }
  const keys = await generateInstallationKeys();
  const installationId = await registerInstallationIdentity(keys.publicKey);
  const next = mergeAuthState({
    installationId,
    publicKey: keys.publicKey,
    privateKey: keys.privateKey,
  });
  return {
    installationId,
    publicKey: next.publicKey!,
    privateKey: next.privateKey!,
  };
}

function shouldResetInstallationIdentity(message: string): boolean {
  return /installation|public key|signature/i.test(message || "");
}

function resetInstallationIdentity(): void {
  const state = readAuthState();
  mergeAuthState({
    ...state,
    installationId: null,
    publicKey: null,
    privateKey: null,
  });
}

async function signAuthPayload(
  action: string,
  payload: Record<string, unknown>,
): Promise<{
  installationId: string;
  timestampMs: number;
  nonce: string;
  signature: string;
}> {
  const identity = await ensureInstallationIdentity();
  const timestampMs = Date.now();
  const nonce = randomId();
  const payloadJson = JSON.stringify(payload);
  const body = `${identity.installationId}\n${action}\n${payloadJson}\n${timestampMs}\n${nonce}`;
  const privateKey = await importPrivateKey(identity.privateKey);
  const encodedBody = new TextEncoder().encode(body);
  const signature = bytesToBase64(
    new Uint8Array(
      await crypto.subtle.sign(
        { name: "Ed25519" } as AlgorithmIdentifier,
        privateKey,
        bytesToArrayBuffer(encodedBody),
      ),
    ),
  );
  return {
    installationId: identity.installationId,
    timestampMs,
    nonce,
    signature,
  };
}

function authBearerHeaders(): Record<string, string> {
  const state = readAuthState();
  return state.accessToken
    ? { authorization: `Bearer ${state.accessToken}` }
    : {};
}

async function refreshAccessToken(): Promise<boolean> {
  const state = readAuthState();
  if (!state.refreshToken || !state.installationId) return false;
  const response = await fetch("/v1/auth/session/refresh", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      refreshToken: state.refreshToken,
      installationId: state.installationId,
    }),
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) return false;
  mergeAuthState({
    accessToken: data.accessToken,
    refreshToken: data.refreshToken,
    expiresAt: data.expiresAt,
    refreshExpiresAt: data.refreshExpiresAt,
  });
  return true;
}

export async function routerAuthFetch(
  input: RequestInfo | URL,
  init: RequestInit = {},
): Promise<Response> {
  const headers = new Headers(init.headers || {});
  Object.entries(authBearerHeaders()).forEach(([key, value]) =>
    headers.set(key, value),
  );
  let response = await fetch(input, { ...init, headers });
  if (response.status === 401 && (await refreshAccessToken())) {
    const retryHeaders = new Headers(init.headers || {});
    Object.entries(authBearerHeaders()).forEach(([key, value]) =>
      retryHeaders.set(key, value),
    );
    response = await fetch(input, { ...init, headers: retryHeaders });
  }
  return response;
}

export async function getRouterSessionStatus(): Promise<RouterSessionStatus> {
  const state = readAuthState();
  const params = new URLSearchParams();
  if (state.installationId) params.set("installationId", state.installationId);
  const response = await routerAuthFetch(
    `/v1/auth/session/me${params.toString() ? `?${params}` : ""}`,
    { cache: "no-store" },
  );
  if (!response.ok) return { authenticated: false };
  return response.json() as Promise<RouterSessionStatus>;
}

export async function requestRouterEmailCode(
  email: string,
): Promise<{ maskedDestination: string; cooldownSecs?: number }> {
  const normalizedEmail = email.trim().toLowerCase();
  const signed = await signAuthPayload("auth_request_code", {
    email: normalizedEmail,
    purpose: "login",
  });
  const response = await fetch("/v1/auth/email/request-code", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email: normalizedEmail, ...signed }),
  });
  return parseJsonResponse(response);
}

export async function requestRouterEmailCodeWithIdentityRetry(
  email: string,
): Promise<{ maskedDestination: string; cooldownSecs?: number }> {
  try {
    return await requestRouterEmailCode(email);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!shouldResetInstallationIdentity(message)) throw error;
    resetInstallationIdentity();
    return requestRouterEmailCode(email);
  }
}

export async function verifyRouterEmailCode(
  email: string,
  code: string,
): Promise<RouterSessionStatus> {
  const identity = await ensureInstallationIdentity();
  const response = await fetch("/v1/auth/email/verify-code", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      email: email.trim().toLowerCase(),
      code: code.trim(),
      installationId: identity.installationId,
    }),
  });
  const data = await parseJsonResponse<{
    user?: { id: string; email: string };
    accessToken: string;
    refreshToken: string;
    expiresAt: string;
    refreshExpiresAt: string;
  }>(response);
  mergeAuthState({
    email: data.user?.email || email.trim().toLowerCase(),
    accessToken: data.accessToken,
    refreshToken: data.refreshToken,
    expiresAt: data.expiresAt,
    refreshExpiresAt: data.refreshExpiresAt,
  });
  return getRouterSessionStatus();
}
