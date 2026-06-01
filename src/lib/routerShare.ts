import { routerAuthFetch } from "@/lib/routerAuth";

export interface RouterShareSettingsPatch {
  ownerEmail?: string;
  description?: string | null;
  forSale?: "Yes" | "No" | "Free";
  marketAccessMode?: "selected" | "all";
  sharedWithEmails?: string[];
  forSaleOfficialPricePercentByApp?: Record<string, number>;
  tokenLimit?: number;
  parallelLimit?: number;
  expiresAt?: string;
  autoStart?: boolean;
}

export interface RouterShareSettingsUpdateResponse {
  ok: boolean;
  appliedSynchronously: boolean;
  edit: {
    id: string;
    shareId: string;
    revision: number;
    status: string;
  };
}

async function parseJsonResponse<T>(response: Response): Promise<T> {
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(data?.message || data?.error || `HTTP ${response.status}`);
  }
  return data as T;
}

export async function updateRouterShareSettings(
  shareId: string,
  patch: RouterShareSettingsPatch,
): Promise<RouterShareSettingsUpdateResponse> {
  return parseJsonResponse(
    await routerAuthFetch(
      `/v1/shares/${encodeURIComponent(shareId)}/settings`,
      {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ patch }),
      },
    ),
  );
}
