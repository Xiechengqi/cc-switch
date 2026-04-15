import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Loader2,
  LogOut,
  ExternalLink,
  Copy,
  Check,
  Plus,
  X,
  Sparkles,
  User,
} from "lucide-react";
import { useClaudeOauth } from "./hooks/useClaudeOauth";
import { copyText } from "@/lib/clipboard";

interface ClaudeOAuthSectionProps {
  className?: string;
  /** 当前选中的 Claude 账号 ID */
  selectedAccountId?: string | null;
  /** 账号选择回调 */
  onAccountSelect?: (accountId: string | null) => void;
}

/**
 * Claude OAuth 认证区块
 *
 * 通过 Anthropic OAuth PKCE 浏览器流程登录 Claude 官方订阅账号，
 * 用于在本地代理模式下使用 Claude.ai 的官方订阅额度。
 */
export const ClaudeOAuthSection: React.FC<ClaudeOAuthSectionProps> = ({
  className,
  selectedAccountId,
  onAccountSelect,
}) => {
  const { t } = useTranslation();
  const [copied, setCopied] = React.useState(false);

  const {
    accounts,
    defaultAccountId,
    hasAnyAccount,
    authState,
    deviceCode,
    error,
    isWaitingBrowser,
    isAddingAccount,
    isRemovingAccount,
    isSettingDefaultAccount,
    addAccount,
    removeAccount,
    setDefaultAccount,
    cancelAuth,
    logout,
  } = useClaudeOauth();

  const copyVerificationUrl = async () => {
    if (!deviceCode?.verification_uri) {
      return;
    }
    await copyText(deviceCode.verification_uri);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleAccountSelect = (value: string) => {
    onAccountSelect?.(value === "none" ? null : value);
  };

  const handleRemoveAccount = (accountId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    e.preventDefault();
    removeAccount(accountId);
    if (selectedAccountId === accountId) {
      onAccountSelect?.(null);
    }
  };

  return (
    <div className={`space-y-4 ${className || ""}`}>
      {/* 认证状态标题 */}
      <div className="flex items-center justify-between">
        <Label>{t("claudeOauth.authStatus", "Claude 订阅认证")}</Label>
        <Badge
          variant={hasAnyAccount ? "default" : "secondary"}
          className={hasAnyAccount ? "bg-green-500 hover:bg-green-600" : ""}
        >
          {hasAnyAccount
            ? t("claudeOauth.accountCount", {
                count: accounts.length,
                defaultValue: `${accounts.length} 个账号`,
              })
            : t("claudeOauth.notAuthenticated", "未认证")}
        </Badge>
      </div>

      {/* 账号选择器 */}
      {hasAnyAccount && onAccountSelect && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("claudeOauth.selectAccount", "选择账号")}
          </Label>
          <Select
            value={selectedAccountId || "none"}
            onValueChange={handleAccountSelect}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={t(
                  "claudeOauth.selectAccountPlaceholder",
                  "选择一个 Claude 账号",
                )}
              />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">
                <span className="text-muted-foreground">
                  {t("claudeOauth.useDefaultAccount", "使用默认账号")}
                </span>
              </SelectItem>
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

      {/* 已登录账号列表 */}
      {hasAnyAccount && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("claudeOauth.loggedInAccounts", "已登录账号")}
          </Label>
          <div className="space-y-1">
            {accounts.map((account) => (
              <div
                key={account.id}
                className="flex items-center justify-between p-2 rounded-md border bg-muted/30"
              >
                <div className="flex items-center gap-2">
                  <User className="h-5 w-5 text-muted-foreground" />
                  <span className="text-sm font-medium">{account.login}</span>
                  {defaultAccountId === account.id && (
                    <Badge variant="secondary" className="text-xs">
                      {t("claudeOauth.defaultAccount", "默认")}
                    </Badge>
                  )}
                  {selectedAccountId === account.id && (
                    <Badge variant="outline" className="text-xs">
                      {t("claudeOauth.selected", "已选中")}
                    </Badge>
                  )}
                </div>
                <div className="flex items-center gap-1">
                  {defaultAccountId !== account.id && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-xs text-muted-foreground"
                      onClick={() => setDefaultAccount(account.id)}
                      disabled={isSettingDefaultAccount}
                    >
                      {t("claudeOauth.setAsDefault", "设为默认")}
                    </Button>
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 text-muted-foreground hover:text-red-500"
                    onClick={(e) => handleRemoveAccount(account.id, e)}
                    disabled={isRemovingAccount}
                    title={t("claudeOauth.removeAccount", "移除账号")}
                  >
                    <X className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* 未认证 - 登录按钮 */}
      {!hasAnyAccount && authState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
        >
          <Sparkles className="mr-2 h-4 w-4" />
          {t("claudeOauth.loginWithClaude", "使用 Claude.ai 登录")}
        </Button>
      )}

      {/* 已有账号 - 添加更多按钮 */}
      {hasAnyAccount && authState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
          disabled={isAddingAccount}
        >
          <Plus className="mr-2 h-4 w-4" />
          {t("claudeOauth.addAnotherAccount", "添加其他账号")}
        </Button>
      )}

      {/* 等待浏览器授权状态 */}
      {isWaitingBrowser && deviceCode && (
        <div className="space-y-3 p-4 rounded-lg border border-border bg-muted/50">
          <div className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t(
              "claudeOauth.waitingForBrowser",
              "请手动打开下方授权链接并完成登录...",
            )}
          </div>

          <div className="rounded-md border bg-background/80 p-3">
            <p className="mb-2 text-xs text-muted-foreground">
              {t(
                "claudeOauth.openLinkHint",
                "授权链接不会自动打开，请点击或复制后在浏览器中访问：",
              )}
            </p>
            <div className="flex items-center gap-2">
              <a
                href={deviceCode.verification_uri}
                target="_blank"
                rel="noopener noreferrer"
                className="min-w-0 flex-1 truncate text-sm text-blue-500 hover:underline"
                title={deviceCode.verification_uri}
              >
                {deviceCode.verification_uri}
              </a>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                onClick={copyVerificationUrl}
                title={t("claudeOauth.copyLink", "复制链接")}
              >
                {copied ? (
                  <Check className="h-4 w-4 text-green-500" />
                ) : (
                  <Copy className="h-4 w-4" />
                )}
              </Button>
              <a
                href={deviceCode.verification_uri}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex"
              >
                <Button type="button" variant="outline" size="sm">
                  {t("claudeOauth.openManually", "打开链接")}
                  <ExternalLink className="ml-1 h-3 w-3" />
                </Button>
              </a>
            </div>
          </div>

          <div className="text-center">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={cancelAuth}
            >
              {t("common.cancel", "取消")}
            </Button>
          </div>
        </div>
      )}

      {/* 错误状态 */}
      {authState === "error" && error && (
        <div className="space-y-2">
          <p className="text-sm text-red-500">{error}</p>
          <div className="flex gap-2">
            <Button
              type="button"
              onClick={addAccount}
              variant="outline"
              size="sm"
            >
              {t("claudeOauth.retry", "重试")}
            </Button>
            <Button
              type="button"
              onClick={cancelAuth}
              variant="ghost"
              size="sm"
            >
              {t("common.cancel", "取消")}
            </Button>
          </div>
        </div>
      )}

      {/* 注销所有账号 */}
      {hasAnyAccount && accounts.length > 1 && (
        <Button
          type="button"
          variant="outline"
          onClick={logout}
          className="w-full text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-950"
        >
          <LogOut className="mr-2 h-4 w-4" />
          {t("claudeOauth.logoutAll", "注销所有账号")}
        </Button>
      )}
    </div>
  );
};

export default ClaudeOAuthSection;
