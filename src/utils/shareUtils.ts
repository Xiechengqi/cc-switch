import type { ConnectInfo, ShareRecord, TunnelConfig, TunnelInfo } from "@/lib/api";
import type { Settings } from "@/types";
export type ShareAction =
  | "enable"
  | "disable"
  | "delete"
  | "connectInfo";

export type ShareTunnelRuntimeStatus =
  | "running"
  | "reconnecting"
  | "stopped"
  | "offline"
  | "unknown";

export function formatShareStatus(status: string): string {
  return status.replace(/_/g, " ");
}

export function getShareUsageRatio(share: Pick<ShareRecord, "tokenLimit" | "tokensUsed">): number {
  if (!share.tokenLimit || share.tokenLimit <= 0) return 0;
  return Math.max(0, Math.min(share.tokensUsed / share.tokenLimit, 1));
}

export function isTunnelConfigured(settings?: Settings | null): boolean {
  const config = getTunnelConfigFromSettings(settings);
  return Boolean(config.domain);
}

export function getTunnelConfigFromSettings(settings?: Settings | null): TunnelConfig {
  return {
    domain: settings?.portrDomain ?? "127.0.0.1:8787",
  };
}

export function buildDefaultShareSubdomain(shareId: string): string {
  return `share-${shareId.slice(0, 8)}`;
}

export function resolveShareTunnelInfo(
  share: Pick<ShareRecord, "id" | "subdomain" | "tunnelUrl">,
  config?: TunnelConfig | null,
): { subdomain: string; tunnelUrl: string } {
  const subdomain = share.subdomain || buildDefaultShareSubdomain(share.id);
  if (share.tunnelUrl) {
    return {
      subdomain,
      tunnelUrl: share.tunnelUrl,
    };
  }
  if (!config?.domain) {
    return {
      subdomain,
      tunnelUrl: "",
    };
  }

  const host = config.domain.split(":")[0] ?? config.domain;
  const isLocal = host === "localhost" || host === "127.0.0.1" || host === "0.0.0.0";
  const protocol = isLocal ? "http" : "https";
  return {
    subdomain,
    tunnelUrl: `${protocol}://${subdomain}.${config.domain}`,
  };
}

export function buildShareCurlExample(connectInfo: ConnectInfo): string {
  return `curl -H "X-API-Key: ${connectInfo.apiKey}" "${connectInfo.tunnelUrl}"`;
}

export function getShareTunnelRuntimeStatus(
  share: Pick<ShareRecord, "status" | "tunnelUrl">,
  tunnelStatus?: TunnelInfo | null,
): ShareTunnelRuntimeStatus {
  if (tunnelStatus?.healthy) {
    return "running";
  }
  if (tunnelStatus && !tunnelStatus.healthy) {
    return "reconnecting";
  }
  if (share.status === "active") {
    return "offline";
  }
  if (share.tunnelUrl) {
    return "stopped";
  }
  return "unknown";
}

export function maskSensitive(value?: string | null, visible = 4): string {
  if (!value) return "";
  if (value.length <= visible) return "*".repeat(value.length);
  return `${"*".repeat(Math.max(4, value.length - visible))}${value.slice(-visible)}`;
}

export function formatUtcDateTime(value?: string | number | null): string {
  if (value == null || value === "") return "-";
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  const parts = new Intl.DateTimeFormat(undefined, {
    timeZone: "UTC",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).formatToParts(date);
  const pick = (type: string) => parts.find((part) => part.type === type)?.value ?? "00";
  return `${pick("year")}-${pick("month")}-${pick("day")} ${pick("hour")}:${pick("minute")}:${pick("second")} UTC`;
}

export function isShareActionAllowed(
  share: ShareRecord,
  action: ShareAction,
  tunnelConfigured: boolean,
  tunnelStatus?: TunnelInfo | null,
): boolean {
  switch (action) {
    case "enable":
      return tunnelConfigured && (share.status !== "active" || !tunnelStatus);
    case "disable":
      return share.status === "active" || Boolean(tunnelStatus || share.tunnelUrl);
    case "delete":
    case "connectInfo":
      return true;
    default:
      return false;
  }
}
