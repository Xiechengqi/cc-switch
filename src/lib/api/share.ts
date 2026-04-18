import { invoke } from "@tauri-apps/api/core";
export interface ShareRecord {
  id: string;
  name: string;
  description?: string | null;
  forSale: "Yes" | "No" | "Free";
  shareToken: string;
  appType: string;
  providerId?: string | null;
  apiKey: string;
  settingsConfig?: string | null;
  tokenLimit: number;
  tokensUsed: number;
  requestsCount: number;
  expiresAt: string;
  subdomain?: string | null;
  tunnelUrl?: string | null;
  status: string;
  createdAt: string;
  lastUsedAt?: string | null;
}

export interface CreateShareParams {
  name: string;
  description?: string;
  forSale: "Yes" | "No" | "Free";
  tokenLimit: number;
  expiresInSecs: number;
  subdomain?: string;
  apiKey?: string;
}

export interface UpdateShareTokenLimitParams {
  shareId: string;
  tokenLimit: number;
}

export interface UpdateShareSubdomainParams {
  shareId: string;
  subdomain: string;
}

export interface UpdateShareApiKeyParams {
  shareId: string;
  apiKey: string;
}

export interface UpdateShareDescriptionParams {
  shareId: string;
  description?: string;
}

export interface UpdateShareForSaleParams {
  shareId: string;
  forSale: "Yes" | "No" | "Free";
}

export interface UpdateShareExpirationParams {
  shareId: string;
  expiresAt: string;
}

export interface TunnelInfo {
  tunnelUrl: string;
  subdomain: string;
  remotePort: number;
  healthy: boolean;
}

export interface TunnelConfig {
  domain: string;
}

export interface ConnectInfo {
  tunnelUrl: string;
  apiKey: string;
  subdomain: string;
}

async function create(params: CreateShareParams): Promise<ShareRecord> {
  return invoke<ShareRecord>("create_share", { params });
}

async function remove(shareId: string): Promise<void> {
  return invoke("delete_share", { shareId });
}

async function pause(shareId: string): Promise<void> {
  return invoke("pause_share", { shareId });
}

async function resume(shareId: string): Promise<void> {
  return invoke("resume_share", { shareId });
}

async function enable(shareId: string): Promise<TunnelInfo> {
  return invoke<TunnelInfo>("enable_share", { shareId });
}

async function disable(shareId: string): Promise<void> {
  return invoke("disable_share", { shareId });
}

async function resetUsage(shareId: string): Promise<ShareRecord> {
  return invoke<ShareRecord>("reset_share_usage", { shareId });
}

async function updateTokenLimit(
  params: UpdateShareTokenLimitParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_token_limit", { params });
}

async function updateSubdomain(
  params: UpdateShareSubdomainParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_subdomain", { params });
}

async function updateApiKey(
  params: UpdateShareApiKeyParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_api_key", { params });
}

async function updateDescription(
  params: UpdateShareDescriptionParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_description", { params });
}

async function updateForSale(
  params: UpdateShareForSaleParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_for_sale", { params });
}

async function updateExpiration(
  params: UpdateShareExpirationParams,
): Promise<ShareRecord> {
  return invoke<ShareRecord>("update_share_expiration", { params });
}

async function list(): Promise<ShareRecord[]> {
  return invoke<ShareRecord[]>("list_shares");
}

async function getDetail(shareId: string): Promise<ShareRecord | null> {
  return invoke<ShareRecord | null>("get_share_detail", { shareId });
}

async function startTunnel(shareId: string): Promise<TunnelInfo> {
  return invoke<TunnelInfo>("start_share_tunnel", { shareId });
}

async function stopTunnel(shareId: string): Promise<void> {
  return invoke("stop_share_tunnel", { shareId });
}

async function getTunnelStatus(shareId: string): Promise<TunnelInfo | null> {
  return invoke<TunnelInfo | null>("get_tunnel_status", { shareId });
}

async function getConnectInfo(shareId: string): Promise<ConnectInfo> {
  return invoke<ConnectInfo>("get_share_connect_info", { shareId });
}

async function configureTunnel(config: TunnelConfig): Promise<void> {
  return invoke("configure_tunnel", { config });
}

export const shareApi = {
  create,
  delete: remove,
  pause,
  resume,
  enable,
  disable,
  resetUsage,
  updateTokenLimit,
  updateSubdomain,
  updateApiKey,
  updateDescription,
  updateForSale,
  updateExpiration,
  list,
  getDetail,
  startTunnel,
  stopTunnel,
  getTunnelStatus,
  getConnectInfo,
  configureTunnel,
};

export const createShare = create;
export const deleteShare = remove;
export const listShares = list;
export const getShareDetail = getDetail;
export const startShareTunnel = startTunnel;
export const stopShareTunnel = stopTunnel;
export const getShareConnectInfo = getConnectInfo;
