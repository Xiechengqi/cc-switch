import React from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { KeyRound, Loader2, Plus, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useOllamaCloud } from "./hooks/useOllamaCloud";

interface OllamaCloudApiKeySectionProps {
  className?: string;
  selectedAccountId?: string | null;
  onAccountSelect?: (accountId: string | null) => void;
  showStoredKeys?: boolean;
}

export const OllamaCloudApiKeySection: React.FC<
  OllamaCloudApiKeySectionProps
> = ({
  className,
  selectedAccountId,
  onAccountSelect,
  showStoredKeys = true,
}) => {
  const { t } = useTranslation();
  const [apiKey, setApiKey] = React.useState("");
  const [label, setLabel] = React.useState("");
  const [showForm, setShowForm] = React.useState(false);
  const {
    accounts,
    hasAnyAccount,
    error,
    isImportingApiKey,
    isRemovingAccount,
    isSettingDefaultAccount,
    isTestingConnection,
    defaultAccountId,
    importApiKey,
    removeAccount,
    setDefaultAccount,
    testConnection,
  } = useOllamaCloud();

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    const nextApiKey = apiKey.trim();
    if (!nextApiKey) return;
    try {
      const account = await importApiKey({
        apiKey: nextApiKey,
        label: label.trim() || null,
      });
      onAccountSelect?.(account.id);
      setApiKey("");
      setLabel("");
      setShowForm(false);
    } catch {
      // Error is surfaced by the hook.
    }
  };

  const handleTestConnection = async () => {
    const nextApiKey = apiKey.trim();
    if (!nextApiKey) return;
    try {
      const models = await testConnection(nextApiKey);
      toast.success(
        t("ollamaCloud.testConnectionSuccess", {
          count: models.length,
          defaultValue: `Ollama Cloud API Key 可用，获取到 ${models.length} 个模型`,
        }),
      );
    } catch {
      // Error is surfaced by the hook.
    }
  };

  return (
    <div className={`space-y-4 ${className || ""}`}>
      <div className="flex items-center justify-between">
        <Label>{t("ollamaCloud.authStatus", "Ollama Cloud API Key")}</Label>
        <Badge
          variant={hasAnyAccount ? "default" : "secondary"}
          className={hasAnyAccount ? "bg-green-500 hover:bg-green-600" : ""}
        >
          {hasAnyAccount
            ? t("ollamaCloud.keyCount", {
                count: accounts.length,
                defaultValue: `${accounts.length} 个 Key`,
              })
            : t("ollamaCloud.notConfigured", "未配置")}
        </Badge>
      </div>

      {hasAnyAccount && showStoredKeys && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("ollamaCloud.storedKeys", "已保存 Key")}
          </Label>
          <div className="space-y-1">
            {accounts.map((account) => (
              <div
                key={account.id}
                className="flex items-center justify-between rounded-md border bg-muted/30 p-2"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <KeyRound className="h-5 w-5 shrink-0 text-muted-foreground" />
                  <div className="min-w-0">
                    <div className="truncate text-sm font-medium">
                      {account.label || account.maskedKey}
                    </div>
                    {account.label && (
                      <div className="truncate text-xs text-muted-foreground">
                        {account.maskedKey}
                      </div>
                    )}
                  </div>
                  {defaultAccountId === account.id && (
                    <Badge variant="secondary" className="text-xs">
                      {t("ollamaCloud.defaultKey", "默认")}
                    </Badge>
                  )}
                  {selectedAccountId === account.id && (
                    <Badge variant="outline" className="text-xs">
                      {t("ollamaCloud.selected", "已选中")}
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
                      onClick={() => setDefaultAccount(account.id)}
                      disabled={isSettingDefaultAccount}
                    >
                      {t("ollamaCloud.setAsDefault", "设为默认")}
                    </Button>
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 text-muted-foreground hover:text-red-500"
                    onClick={() => {
                      removeAccount(account.id);
                      if (selectedAccountId === account.id) {
                        onAccountSelect?.(null);
                      }
                    }}
                    disabled={isRemovingAccount}
                    title={t("ollamaCloud.removeKey", "移除 Key")}
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
            {t("ollamaCloud.selectKey", "选择 Key")}
          </Label>
          <Select
            value={selectedAccountId || "none"}
            onValueChange={(value) =>
              onAccountSelect(value === "none" ? null : value)
            }
          >
            <SelectTrigger>
              <SelectValue
                placeholder={t(
                  "ollamaCloud.selectKeyPlaceholder",
                  "选择一个 Ollama Cloud API Key",
                )}
              />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">
                <span className="text-muted-foreground">
                  {t("ollamaCloud.useDefaultKey", "使用默认 Key")}
                </span>
              </SelectItem>
              {accounts.map((account) => (
                <SelectItem key={account.id} value={account.id}>
                  {account.label || account.maskedKey}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}

      {showForm && (
        <form className="space-y-3" onSubmit={handleSubmit}>
          <div className="space-y-2">
            <Label htmlFor="ollama-cloud-label">
              {t("ollamaCloud.label", "标签")}
            </Label>
            <Input
              id="ollama-cloud-label"
              value={label}
              onChange={(event) => setLabel(event.currentTarget.value)}
              placeholder={t(
                "ollamaCloud.labelPlaceholder",
                "例如 Personal / Team",
              )}
              autoComplete="off"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="ollama-cloud-api-key">
              {t("ollamaCloud.apiKey", "API Key")}
            </Label>
            <Input
              id="ollama-cloud-api-key"
              type="password"
              value={apiKey}
              onChange={(event) => setApiKey(event.currentTarget.value)}
              autoComplete="off"
              required
            />
          </div>
          {error && <p className="text-sm text-red-500">{error}</p>}
          <div className="flex gap-2">
            <Button
              type="submit"
              variant="outline"
              className="flex-1"
              disabled={isImportingApiKey || !apiKey.trim()}
            >
              {isImportingApiKey ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <Plus className="mr-2 h-4 w-4" />
              )}
              {t("ollamaCloud.saveKey", "保存 Key")}
            </Button>
            <Button
              type="button"
              variant="ghost"
              disabled={isTestingConnection || !apiKey.trim()}
              onClick={handleTestConnection}
            >
              {isTestingConnection && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              {t("ollamaCloud.testKey", "测试")}
            </Button>
          </div>
        </form>
      )}

      {!showForm && (
        <Button type="button" variant="outline" onClick={() => setShowForm(true)}>
          <Plus className="mr-2 h-4 w-4" />
          {hasAnyAccount
            ? t("ollamaCloud.addAnotherKey", "添加其他 Key")
            : t("ollamaCloud.addKey", "添加 Key")}
        </Button>
      )}
    </div>
  );
};
