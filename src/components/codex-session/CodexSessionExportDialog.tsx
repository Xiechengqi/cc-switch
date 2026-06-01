import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";
import { Loader2, FileUp, Copy, AlertTriangle } from "lucide-react";
import { toast } from "sonner";
import { save } from "@tauri-apps/plugin-dialog";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

import {
  CodexExportFormat,
  CodexSessionExportResult,
  exportCodexSessions,
  saveCodexSessionExport,
} from "@/lib/api/codexSession";
import {
  authListAccounts,
  ManagedAuthAccount,
} from "@/lib/api/auth";
import { copyText } from "@/lib/clipboard";

interface CodexSessionExportDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const FORMAT_OPTIONS = (t: (k: string, fallback: string) => string): {
  value: CodexExportFormat;
  label: string;
  hint: string;
}[] => [
  { value: "codex_cli", label: t("codexSession.formatCodexCli", "Codex CLI auth.json"), hint: t("codexSession.formatCodexCliHint", "放到目标机器 ~/.codex/") },
  { value: "cpa", label: t("codexSession.formatCpa", "CLIProxyAPI"), hint: t("codexSession.formatCpaHint", "放到 CPA auths/ 目录") },
  { value: "sub2api", label: t("codexSession.formatSub2api", "sub2api 导入包"), hint: t("codexSession.formatSub2apiHint", "贴到 sub2api 管理后台或用 curl") },
  { value: "raw_jwt", label: t("codexSession.formatRawJwt", "裸 access_token"), hint: t("codexSession.formatRawJwtHint", "一次性使用，无 refresh") },
  { value: "cc_switch_envelope", label: t("codexSession.formatEnvelope", "cc-switch 备份"), hint: t("codexSession.formatEnvelopeHint", "完整备份，跨设备迁移") },
];

/**
 * Export managed Codex OAuth sessions to any supported wire format.
 *
 * UX:
 * 1. Multi-select accounts (defaults to "all").
 * 2. Pick target format (per-item or batch envelope).
 * 3. Optional flags: refresh-first, redact, mark-handoff, envelope password.
 * 4. Single-click "Save / Copy" — saves to disk or copies to clipboard.
 *
 * The dialog never displays raw token material; only the post-render
 * payload is shown in the preview area, and the user is expected to save
 * it to a file rather than read it inline.
 */
export function CodexSessionExportDialog({
  open,
  onOpenChange,
}: CodexSessionExportDialogProps) {
  const { t } = useTranslation();
  const accountsQuery = useQuery<ManagedAuthAccount[]>({
    queryKey: ["auth", "codex_oauth"],
    queryFn: () => authListAccounts("codex_oauth"),
    enabled: open,
  });

  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [format, setFormat] = useState<CodexExportFormat>("codex_cli");
  const [refreshFirst, setRefreshFirst] = useState(true);
  const [redact, setRedact] = useState(false);
  const [markHandoff, setMarkHandoff] = useState(false);
  const [password, setPassword] = useState("");
  const [exporting, setExporting] = useState(false);
  const [result, setResult] = useState<CodexSessionExportResult | null>(null);
  const [pendingConfirm, setPendingConfirm] = useState(false);

  // Conditions under which the exported file lets a downstream consumer
  // rotate refresh_token — exactly the situations where two managers running
  // on the same session will silently poison each other. Surfaces a modal
  // confirm step rather than a toast, because users routinely dismiss toasts
  // before reading them.
  const rotationHazard =
    !redact &&
    selectedIds.length > 0 &&
    (format === "codex_cli" ||
      format === "cpa" ||
      format === "sub2api" ||
      format === "cc_switch_envelope") &&
    !markHandoff;

  // Default selection = all accounts whenever the list refreshes.
  useEffect(() => {
    if (accountsQuery.data && selectedIds.length === 0) {
      setSelectedIds(accountsQuery.data.map((a) => a.id));
    }
  }, [accountsQuery.data]);

  const accounts = accountsQuery.data ?? [];
  const passwordAllowed = format === "cc_switch_envelope";
  const passwordToSend = passwordAllowed ? password.trim() : "";

  const reset = () => {
    setResult(null);
    setPassword("");
    setRedact(false);
    setMarkHandoff(false);
    setPendingConfirm(false);
  };

  const doExport = async () => {
    setExporting(true);
    try {
      const r = await exportCodexSessions({
        accountIds: selectedIds,
        format,
        refreshFirst,
        redact,
        markHandoff,
        password: passwordToSend || undefined,
      });
      setResult(r);
      if (r.warnings.length > 0) {
        r.warnings.forEach((w) => toast.warning(w));
      }
    } catch (e) {
      toast.error(String(e));
    } finally {
      setExporting(false);
    }
  };

  const handleExport = async () => {
    if (selectedIds.length === 0) {
      toast.error(t("codexSession.selectAccountFirst", "请至少选择一个账号"));
      return;
    }
    if (redact && passwordToSend) {
      toast.error(t("codexSession.redactExclusivePassword", "redact 与密码加密互斥"));
      return;
    }
    if (rotationHazard) {
      setPendingConfirm(true);
      return;
    }
    await doExport();
  };

  const handleSaveToFile = async () => {
    if (!result) return;
    try {
      const path = await save({
        defaultPath: result.suggestedFilename,
        filters: [
          { name: "Codex session", extensions: ["json", "jsonl", "jwt", "txt"] },
        ],
      });
      if (!path) return;
      await saveCodexSessionExport(path, result.payload);
      toast.success(t("codexSession.savedToToast", { defaultValue: "已保存到 {{path}}", path }));
    } catch (e) {
      toast.error(String(e));
    }
  };

  const handleCopyPayload = async () => {
    if (!result) return;
    await copyText(result.payload);
    toast.success(t("codexSession.copiedToast", "已复制到剪贴板"));
  };

  const handleCopyCurl = async () => {
    if (!result?.curlCommand) return;
    await copyText(result.curlCommand);
    toast.success(t("codexSession.curlCopiedToast", "已复制 curl 命令"));
  };

  const toggleAll = () => {
    if (selectedIds.length === accounts.length) {
      setSelectedIds([]);
    } else {
      setSelectedIds(accounts.map((a) => a.id));
    }
  };

  return (
    <>
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-[720px]">
        <DialogHeader>
          <DialogTitle>{t("codexSession.exportTitle", "导出 Codex Session")}</DialogTitle>
          <DialogDescription>
            {t("codexSession.exportDescription", "从托管账号池导出供下游消费方使用。导出前会自动 refresh 至最新 access_token。")}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          {/* 账号选择 */}
          <div className="border rounded-md">
            <div className="px-3 py-2 text-xs flex items-center justify-between border-b bg-muted/30">
              <span>
                {t("codexSession.exportAccountsLabel", {
                  defaultValue: "账号 ({{selected}}/{{total}})",
                  selected: selectedIds.length,
                  total: accounts.length,
                })}
              </span>
              <Button variant="ghost" size="sm" onClick={toggleAll}>
                {selectedIds.length === accounts.length
                  ? t("codexSession.deselectAll", "取消全选")
                  : t("codexSession.selectAll", "全选")}
              </Button>
            </div>
            <ScrollArea className="max-h-[140px]">
              {accountsQuery.isLoading && (
                <div className="px-3 py-2 text-xs text-muted-foreground">{t("codexSession.loadingAccounts", "加载中...")}</div>
              )}
              {accounts.length === 0 && !accountsQuery.isLoading && (
                <div className="px-3 py-2 text-xs text-muted-foreground">
                  {t("codexSession.noManagedAccounts", "当前没有托管账号；先通过 ChatGPT 登录或导入 session")}
                </div>
              )}
              {accounts.map((acc) => (
                <label key={acc.id} className="flex items-center gap-2 px-3 py-1 text-xs hover:bg-muted/50 cursor-pointer">
                  <Checkbox
                    checked={selectedIds.includes(acc.id)}
                    onCheckedChange={(checked) => {
                      setSelectedIds((prev) =>
                        checked ? [...prev, acc.id] : prev.filter((id) => id !== acc.id),
                      );
                    }}
                  />
                  <span className="flex-1">{acc.email ?? acc.login}</span>
                  <span className="font-mono text-[10px] text-muted-foreground">{acc.id.slice(0, 14)}</span>
                </label>
              ))}
            </ScrollArea>
          </div>

          {/* 格式 */}
          <div className="space-y-1">
            <Label className="text-xs">{t("codexSession.formatLabel", "目标格式")}</Label>
            <Select value={format} onValueChange={(v) => setFormat(v as CodexExportFormat)}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                {FORMAT_OPTIONS(t as any).map((opt) => (
                  <SelectItem key={opt.value} value={opt.value}>
                    <div className="flex flex-col">
                      <span>{opt.label}</span>
                      <span className="text-[10px] text-muted-foreground">{opt.hint}</span>
                    </div>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {/* 选项 */}
          <div className="grid grid-cols-2 gap-3">
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2">
              <Label className="text-xs">{t("codexSession.optRefreshFirst", "导出前自动 refresh")}</Label>
              <Switch checked={refreshFirst} onCheckedChange={setRefreshFirst} />
            </div>
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2">
              <Label className="text-xs">{t("codexSession.optRedact", "redact 模式")}</Label>
              <Switch checked={redact} onCheckedChange={setRedact} />
            </div>
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2 col-span-2">
              <div>
                <Label className="text-xs">{t("codexSession.optMarkHandoff", "导出后标记为交接态")}</Label>
                <div className="text-[10px] text-muted-foreground">{t("codexSession.optMarkHandoffHint", "本地停止 refresh，避免与下游消费方发生轮换冲突")}</div>
              </div>
              <Switch checked={markHandoff} onCheckedChange={setMarkHandoff} />
            </div>
            {passwordAllowed && (
              <div className="col-span-2 space-y-1">
                <Label className="text-xs">{t("codexSession.envPasswordOptional", "envelope 密码（可选）")}</Label>
                <Input
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={t("codexSession.envPasswordHintEmpty", "留空则不加密") as string}
                  disabled={redact}
                />
                {redact && password && (
                  <div className="text-[10px] text-destructive flex items-center gap-1">
                    <AlertTriangle className="h-3 w-3" /> {t("codexSession.redactDisablesPassword", "redact 模式下密码会被忽略")}
                  </div>
                )}
              </div>
            )}
          </div>

          {/* 结果 */}
          {result && (
            <div className="space-y-2 border-t pt-2">
              <div className="text-xs space-y-1">
                <div className="flex flex-wrap items-center gap-1">
                  <Badge variant="default">{t("codexSession.countBadge", { defaultValue: "{{n}} 条", n: result.accountCount })}</Badge>
                  <Badge variant="secondary">{result.suggestedFilename}</Badge>
                  {result.redacted && <Badge variant="destructive">{t("codexSession.redactedBadge", "REDACTED")}</Badge>}
                </div>
                {result.warnings.map((w, i) => (
                  <div key={i} className="text-amber-600 dark:text-amber-400 text-[11px] flex items-start gap-1">
                    <AlertTriangle className="h-3 w-3 mt-0.5" /> {w}
                  </div>
                ))}
                {result.items.filter((i) => i.status !== "ok").map((i, k) => (
                  <div key={k} className="text-destructive text-[11px]">
                    {i.accountId}: {i.message ?? i.status}
                  </div>
                ))}
              </div>
              <ScrollArea className="h-[100px] border rounded-md p-2 bg-muted/30">
                <pre className="text-[10px] font-mono whitespace-pre-wrap break-all">
                  {result.payload.length > 2000
                    ? result.payload.slice(0, 2000) + "\n" + t("codexSession.truncatedPreview", "... (截断，请保存到文件)")
                    : result.payload}
                </pre>
              </ScrollArea>
              <div className="flex gap-2">
                <Button size="sm" variant="default" onClick={handleSaveToFile}>
                  <FileUp className="mr-2 h-3 w-3" /> {t("codexSession.saveToFile", "保存为文件")}
                </Button>
                <Button size="sm" variant="outline" onClick={handleCopyPayload}>
                  <Copy className="mr-2 h-3 w-3" /> {t("codexSession.copyPayload", "复制内容")}
                </Button>
                {result.curlCommand && (
                  <Button size="sm" variant="outline" onClick={handleCopyCurl}>
                    <Copy className="mr-2 h-3 w-3" /> {t("codexSession.copyCurl", "复制 curl")}
                  </Button>
                )}
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={exporting}>
            {t("codexSession.btnClose", "关闭")}
          </Button>
          <Button
            onClick={handleExport}
            disabled={exporting || selectedIds.length === 0}
          >
            {exporting ? (
              <><Loader2 className="mr-2 h-4 w-4 animate-spin" /> {t("codexSession.btnExporting", "导出中")}</>
            ) : (
              t("codexSession.btnGenerate", "生成")
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    <ConfirmDialog
      isOpen={pendingConfirm}
      title={t("codexSession.confirmExportTitle", "导出含可用 refresh_token")}
      message={t("codexSession.confirmExportMessage", {
        defaultValue: `即将导出 {{count}} 个账号的可续期 session。\n\n如果接收方也启用了自动 refresh（如另一台 cc-switch、CLIProxyAPI、sub2api），rotating refresh_token 会被两端互相轮换作废，导致两边都需要重新登录。\n\n建议：要么勾选「导出后标记为交接态」让本地停止 refresh，要么确保接收方仅作只读消费方（不调用 refresh 端点）。`,
        count: selectedIds.length,
      }) as string}
      confirmText={t("codexSession.confirmExportProceed", "我了解风险，继续导出") as string}
      cancelText={t("codexSession.confirmExportCancel", "取消") as string}
      variant="destructive"
      onConfirm={() => {
        setPendingConfirm(false);
        void doExport();
      }}
      onCancel={() => setPendingConfirm(false)}
    />
  </>);
}
