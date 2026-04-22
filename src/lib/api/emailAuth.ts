import { invoke } from "@tauri-apps/api/core";

export interface EmailAuthStatus {
  authenticated: boolean;
  email?: string | null;
  expiresAt?: number | null;
}

export interface EmailCodeRequestResponse {
  ok: boolean;
  cooldownSecs: number;
  maskedDestination: string;
}

export interface EmailAuthUser {
  id: string;
  email: string;
}

export interface EmailSessionMeResponse {
  authenticated: boolean;
  user?: EmailAuthUser | null;
  expiresAt?: string | null;
  installationOwnerEmail?: string | null;
}

async function requestCode(email: string): Promise<EmailCodeRequestResponse> {
  return invoke("email_auth_request_code", { email });
}

async function verifyCode(
  email: string,
  code: string,
): Promise<EmailAuthStatus> {
  return invoke("email_auth_verify_code", { email, code });
}

async function getStatus(): Promise<EmailAuthStatus> {
  return invoke("email_auth_get_status");
}

async function sessionMe(): Promise<EmailSessionMeResponse> {
  return invoke("email_auth_session_me");
}

async function logout(): Promise<void> {
  return invoke("email_auth_logout");
}

export const emailAuthApi = {
  requestCode,
  verifyCode,
  getStatus,
  sessionMe,
  logout,
};
