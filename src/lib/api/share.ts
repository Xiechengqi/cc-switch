import { invokeCommand } from "@/lib/runtime";
export interface ShareRecord {
  id: string;
  name: string;
  ownerEmail: string;
  sharedWithEmails: string[];
  marketAccessMode: "selected" | "all";
  forSaleOfficialPricePercentByApp: Record<string, number>;
  description?: string | null;
  forSale: "Yes" | "No" | "Free";
  shareToken: string;
  appType: string;
  providerId?: string | null;
  apiKey: string;
  settingsConfig?: string | null;
  tokenLimit: number;
  parallelLimit: number;
  tokensUsed: number;
  requestsCount: number;
  expiresAt: string;
  subdomain?: string | null;
  tunnelUrl?: string | null;
  status: string;
  autoStart: boolean;
  createdAt: string;
  lastUsedAt?: string | null;
}

export interface CreateShareParams {
  ownerEmail: string;
  appType: "claude" | "codex" | "gemini";
  /**
   * 绑定的 provider id（必填）。share 请求只走该 provider，
   * 不参与 failover，且不会被其他 share 同时绑定。
   */
  providerId: string;
  description?: string;
  forSale: "Yes" | "No" | "Free";
  tokenLimit: number;
  parallelLimit: number;
  expiresInSecs: number;
  subdomain?: string;
  autoStart: boolean;
}

export interface UpdateShareProviderBindingParams {
  shareId: string;
  providerId: string;
}

export interface PublicMarket {
  id: string;
  displayName: string;
  email: string;
  subdomain: string;
  publicBaseUrl: string;
  status: string;
}

export interface UpdateShareAclParams {
  shareId: string;
  sharedWithEmails: string[];
  marketAccessMode: "selected" | "all";
}

export interface UpdateShareTokenLimitParams {
  shareId: string;
  tokenLimit: number;
}

export interface UpdateShareParallelLimitParams {
  shareId: string;
  parallelLimit: number;
}

export interface UpdateShareSubdomainParams {
  shareId: string;
  subdomain: string;
}

export interface UpdateShareDescriptionParams {
  shareId: string;
  description?: string;
}

export interface UpdateShareForSaleParams {
  shareId: string;
  forSale: "Yes" | "No" | "Free";
}

export interface UpdateShareForSaleOfficialPricePercentParams {
  shareId: string;
  pricing: Record<string, number>;
}

export interface UpdateShareExpirationParams {
  shareId: string;
  expiresAt: string;
}

export interface UpdateShareAutoStartParams {
  shareId: string;
  autoStart: boolean;
}

export interface UpdateShareOwnerEmailParams {
  shareId: string;
  ownerEmail: string;
}

export interface TransferShareOwnerParams {
  shareId: string;
  targetEmail: string;
}

export interface TunnelInfo {
  tunnelUrl: string;
  subdomain: string;
  remotePort: number;
  healthy: boolean;
}

export interface ShareTunnelStatus {
  info?: TunnelInfo | null;
  lastError?: string | null;
  requiresOwnerLogin: boolean;
}

export interface TunnelConfig {
  domain: string;
}

export interface ConnectInfo {
  tunnelUrl: string;
  subdomain: string;
}

async function create(params: CreateShareParams): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("create_share", { params });
}

async function remove(shareId: string): Promise<void> {
  return invokeCommand("delete_share", { shareId });
}

async function pause(shareId: string): Promise<void> {
  return invokeCommand("pause_share", { shareId });
}

async function resume(shareId: string): Promise<void> {
  return invokeCommand("resume_share", { shareId });
}

async function enable(shareId: string): Promise<TunnelInfo> {
  return invokeCommand<TunnelInfo>("enable_share", { shareId });
}

async function disable(shareId: string): Promise<void> {
  return invokeCommand("disable_share", { shareId });
}

async function resetUsage(shareId: string): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("reset_share_usage", { shareId });
}

async function updateTokenLimit(
  params: UpdateShareTokenLimitParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_token_limit", { params });
}

async function updateParallelLimit(
  params: UpdateShareParallelLimitParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_parallel_limit", { params });
}

async function updateSubdomain(
  params: UpdateShareSubdomainParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_subdomain", { params });
}

async function updateDescription(
  params: UpdateShareDescriptionParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_description", { params });
}

async function updateForSale(
  params: UpdateShareForSaleParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_for_sale", { params });
}

async function updateForSaleOfficialPricePercent(
  params: UpdateShareForSaleOfficialPricePercentParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_for_sale_official_price_percent", {
    params,
  });
}

async function updateExpiration(
  params: UpdateShareExpirationParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_expiration", { params });
}

async function updateAutoStart(
  params: UpdateShareAutoStartParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_auto_start", { params });
}

async function updateOwnerEmail(
  params: UpdateShareOwnerEmailParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_owner_email", { params });
}

async function transferOwner(
  params: TransferShareOwnerParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("transfer_share_owner", { params });
}

async function updateAcl(params: UpdateShareAclParams): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_acl", { params });
}

async function updateProviderBinding(
  params: UpdateShareProviderBindingParams,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("update_share_provider_binding", { params });
}

async function listMarkets(): Promise<PublicMarket[]> {
  return invokeCommand<PublicMarket[]>("list_share_markets");
}

async function authorizeMarket(
  shareId: string,
  marketEmail: string,
): Promise<ShareRecord> {
  return invokeCommand<ShareRecord>("authorize_share_market", {
    shareId,
    marketEmail,
  });
}

async function list(): Promise<ShareRecord[]> {
  return invokeCommand<ShareRecord[]>("list_shares");
}

async function getDetail(shareId: string): Promise<ShareRecord | null> {
  return invokeCommand<ShareRecord | null>("get_share_detail", { shareId });
}

async function startTunnel(shareId: string): Promise<TunnelInfo> {
  return invokeCommand<TunnelInfo>("start_share_tunnel", { shareId });
}

async function stopTunnel(shareId: string): Promise<void> {
  return invokeCommand("stop_share_tunnel", { shareId });
}

async function getTunnelStatus(shareId: string): Promise<ShareTunnelStatus> {
  return invokeCommand<ShareTunnelStatus>("get_tunnel_status", { shareId });
}

async function getConnectInfo(shareId: string): Promise<ConnectInfo> {
  return invokeCommand<ConnectInfo>("get_share_connect_info", { shareId });
}

async function configureTunnel(config: TunnelConfig): Promise<void> {
  return invokeCommand("configure_tunnel", { config });
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
  updateParallelLimit,
  updateSubdomain,
  updateDescription,
  updateForSale,
  updateForSaleOfficialPricePercent,
  updateExpiration,
  updateAutoStart,
  updateOwnerEmail,
  transferOwner,
  updateAcl,
  updateProviderBinding,
  listMarkets,
  authorizeMarket,
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
