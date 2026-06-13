import { invokeCommand } from "@/lib/runtime";
import type { ManagedAuthAccount } from "./auth";

export interface OpenAISessionStatus {
  authenticated: boolean;
  default_account_id: string | null;
  accounts: ManagedAuthAccount[];
}

export interface OpenAISessionImportOutcome {
  account: ManagedAuthAccount;
  action: "created" | "updated";
  expires_at_ms: number;
}

export async function importOpenAISession(
  sessionJson: string,
): Promise<OpenAISessionImportOutcome> {
  return invokeCommand<OpenAISessionImportOutcome>("openai_session_import", {
    sessionJson,
  });
}

export async function getOpenAISessionStatus(): Promise<OpenAISessionStatus> {
  return invokeCommand<OpenAISessionStatus>("openai_session_status");
}

export async function removeOpenAISession(accountId: string): Promise<void> {
  return invokeCommand("openai_session_remove", { accountId });
}

export async function setDefaultOpenAISession(
  accountId: string,
): Promise<void> {
  return invokeCommand("openai_session_set_default", { accountId });
}

export async function clearOpenAISessions(): Promise<void> {
  return invokeCommand("openai_session_clear");
}

export const openaiSessionApi = {
  importOpenAISession,
  getOpenAISessionStatus,
  removeOpenAISession,
  setDefaultOpenAISession,
  clearOpenAISessions,
};
