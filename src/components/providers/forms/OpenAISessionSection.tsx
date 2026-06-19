import React from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { FileDown, Loader2, User, X } from "lucide-react";
import { useOpenAISession } from "./hooks/useOpenAISession";

interface OpenAISessionSectionProps {
  className?: string;
  selectedAccountId?: string | null;
  onAccountSelect?: (accountId: string | null) => void;
}

export const OpenAISessionSection: React.FC<OpenAISessionSectionProps> = ({
  className,
  selectedAccountId,
  onAccountSelect,
}) => {
  const { t } = useTranslation();
  const [sessionJson, setSessionJson] = React.useState("");
  const {
    accounts,
    hasAnyAccount,
    defaultAccountId,
    importSession,
    removeAccount,
    setDefaultAccount,
    isImporting,
    isRemovingAccount,
    isSettingDefaultAccount,
  } = useOpenAISession();

  const handleImport = async () => {
    const trimmed = sessionJson.trim();
    if (!trimmed) {
      toast.error(
        t("openaiSession.emptyJson", {
          defaultValue:
            "请粘贴 ChatGPT session Cookie、/api/auth/session JSON 或 Codex auth JSON",
        }),
      );
      return;
    }
    try {
      const outcome = await importSession(trimmed);
      setSessionJson("");
      onAccountSelect?.(outcome.account.id);
      toast.success(
        outcome.action === "created"
          ? t("openaiSession.imported", {
              defaultValue: "Session 账号已导入",
            })
          : t("openaiSession.updated", {
              defaultValue: "Session 账号已更新",
            }),
      );
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    }
  };

  const handleRemove = (accountId: string) => {
    removeAccount(accountId);
    if (selectedAccountId === accountId) {
      onAccountSelect?.(null);
    }
  };

  return (
    <div className={`space-y-4 ${className || ""}`}>
      <div className="flex items-center justify-between">
        <Label>{t("openaiSession.title", "OpenAI session")}</Label>
        <Badge
          variant={hasAnyAccount ? "default" : "secondary"}
          className={hasAnyAccount ? "bg-green-500 hover:bg-green-600" : ""}
        >
          {hasAnyAccount
            ? t("openaiSession.accountCount", {
                count: accounts.length,
                defaultValue: `${accounts.length} 个账号`,
              })
            : t("openaiSession.notImported", "未导入")}
        </Badge>
      </div>

      <div className="space-y-2">
        <Textarea
          value={sessionJson}
          onChange={(event) => setSessionJson(event.target.value)}
          rows={6}
          placeholder={t("openaiSession.placeholder", {
            defaultValue:
              "粘贴 __Secure-next-auth.session-token Cookie、https://chatgpt.com/api/auth/session 返回的 JSON，或 Codex auth.json",
          })}
        />
        <Button
          type="button"
          onClick={handleImport}
          disabled={isImporting}
          className="w-full"
        >
          {isImporting ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <FileDown className="mr-2 h-4 w-4" />
          )}
          {t("openaiSession.import", "导入 session")}
        </Button>
      </div>

      {hasAnyAccount && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("openaiSession.accounts", "已导入账号")}
          </Label>
          <div className="space-y-1">
            {accounts.map((account) => (
              <div
                key={account.id}
                className="flex items-center justify-between rounded-md border bg-muted/30 p-2"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <User className="h-5 w-5 shrink-0 text-muted-foreground" />
                  <span className="truncate text-sm font-medium">
                    {account.login}
                  </span>
                  {defaultAccountId === account.id && (
                    <Badge variant="secondary" className="text-xs">
                      {t("openaiSession.defaultAccount", "默认")}
                    </Badge>
                  )}
                  {selectedAccountId === account.id && (
                    <Badge variant="outline" className="text-xs">
                      {t("openaiSession.selected", "已选中")}
                    </Badge>
                  )}
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  {defaultAccountId !== account.id && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-xs text-muted-foreground"
                      disabled={isSettingDefaultAccount}
                      onClick={() => setDefaultAccount(account.id)}
                    >
                      {t("openaiSession.setAsDefault", "设为默认")}
                    </Button>
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 text-muted-foreground hover:text-red-500"
                    disabled={isRemovingAccount}
                    onClick={() => handleRemove(account.id)}
                    title={t("openaiSession.remove", "移除账号")}
                  >
                    <X className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {hasAnyAccount && onAccountSelect && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("openaiSession.selectAccount", "选择 session 账号")}
          </Label>
          <Select
            value={selectedAccountId ?? undefined}
            onValueChange={(value) => onAccountSelect(value)}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={t("openaiSession.selectPlaceholder", {
                  defaultValue: "选择一个 OpenAI session 账号",
                })}
              />
            </SelectTrigger>
            <SelectContent>
              {accounts.map((account) => (
                <SelectItem key={account.id} value={account.id}>
                  <div className="flex items-center gap-2">
                    <User className="h-4 w-4 text-muted-foreground" />
                    <span>{account.login}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}
    </div>
  );
};

export default OpenAISessionSection;
