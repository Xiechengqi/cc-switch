import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Loader2, FileDown, AlertTriangle } from "lucide-react";
import { toast } from "sonner";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";

import {
  CodexSessionImportResult,
  CodexSessionPreviewResult,
  getCodexSessionMachineId,
  importCodexSessions,
  previewCodexSessionParse,
} from "@/lib/api/codexSession";

interface CodexSessionImportDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Called after a successful import so the parent can refresh account list. */
  onImported?: (result: CodexSessionImportResult) => void;
}

const SOURCE_LABELS: Record<string, string> = {
  codex_cli: "Codex CLI",
  cpa: "CLIProxyAPI",
  sub2api: "sub2api",
  raw_jwt: "Raw JWT",
  cc_switch: "cc-switch envelope",
  unknown: "未知格式",
};

const formatExp = (exp: number | null): string => {
  if (!exp) return "—";
  const ms = exp * 1000;
  const d = new Date(ms);
  const now = Date.now();
  const diffMs = ms - now;
  const expired = diffMs < 0;
  const abs = Math.abs(diffMs);
  const mins = Math.round(abs / 60_000);
  const stamp = d.toLocaleString();
  if (expired) return `${stamp} (已过期 ${mins}min)`;
  if (mins < 90) return `${stamp} (${mins}min 后)`;
  const hours = Math.round(mins / 60);
  return `${stamp} (${hours}h 后)`;
};

/**
 * Import Codex sessions pasted from external sources (Codex CLI auth.json,
 * CPA token storage, sub2api wrapper, raw JWTs, JSONL, cc-switch envelope).
 *
 * Layout: paste textarea on top, live preview table in the middle, options
 * (update existing / reject expired / verify refresh / envelope password) at
 * the bottom. Preview parses pure — token material never returns to the UI.
 */
export function CodexSessionImportDialog({
  open,
  onOpenChange,
  onImported,
}: CodexSessionImportDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [content, setContent] = useState("");
  const [preview, setPreview] = useState<CodexSessionPreviewResult | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const [updateExisting, setUpdateExisting] = useState(true);
  const [rejectExpired, setRejectExpired] = useState(true);
  const [verifyRefresh, setVerifyRefresh] = useState(false);
  const [password, setPassword] = useState("");

  const [importing, setImporting] = useState(false);
  const [result, setResult] = useState<CodexSessionImportResult | null>(null);

  const [localMachineId, setLocalMachineId] = useState<string | null>(null);
  useEffect(() => {
    void getCodexSessionMachineId().then(setLocalMachineId);
  }, []);

  const crossMachineWarning = useMemo(() => {
    if (!preview?.envelopeSourceMachineId || !localMachineId) return null;
    if (preview.envelopeSourceMachineId === localMachineId) return null;
    return t("codexSession.crossMachineWarning", {
      id: preview.envelopeSourceMachineId.slice(0, 8),
      defaultValue: `此 envelope 来自其他机器 (${preview.envelopeSourceMachineId.slice(0, 8)})；如果两边同时托管同一份 session，refresh_token 会因轮换互相作废`,
    });
  }, [preview, localMachineId, t]);

  // Debounce preview parsing to avoid IPC thrash while typing.
  const [debounceTimer, setDebounceTimer] = useState<number | null>(null);
  const onContentChange = (text: string) => {
    setContent(text);
    setResult(null);
    if (debounceTimer) window.clearTimeout(debounceTimer);
    if (!text.trim()) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    const handle = window.setTimeout(async () => {
      setPreviewing(true);
      try {
        const r = await previewCodexSessionParse(text);
        setPreview(r);
        setPreviewError(null);
      } catch (e) {
        setPreviewError(String(e));
        setPreview(null);
      } finally {
        setPreviewing(false);
      }
    }, 300);
    setDebounceTimer(handle);
  };

  // Backend already detected the encrypted envelope and flagged it on the
  // preview result; trust that instead of re-parsing JSON client-side.
  const isEncryptedEnvelope = preview?.envelopeEncrypted ?? false;

  const importableCount = useMemo(() => {
    if (!preview) return 0;
    return preview.items.filter(
      (item) => !item.error && item.hasRefreshToken && item.accountId,
    ).length;
  }, [preview]);

  const handleImport = async () => {
    setImporting(true);
    try {
      const r = await importCodexSessions({
        content,
        updateExisting,
        rejectExpired,
        verifyRefresh,
        password: password.trim() || undefined,
      });
      setResult(r);
      // Cache invalidation: the managed accounts list (Codex OAuth) just
      // gained members, so anything keyed on auth_list_accounts should refetch.
      void queryClient.invalidateQueries({ queryKey: ["auth", "codex_oauth"] });
      toast.success(
        t("codexSession.importedToast", {
          defaultValue:
            "导入完成: 新增 {{created}}, 更新 {{updated}}, 跳过 {{skipped}}, 失败 {{failed}}",
          created: r.created,
          updated: r.updated,
          skipped: r.skipped,
          failed: r.failed,
        }),
      );
      onImported?.(r);
    } catch (e) {
      toast.error(String(e));
    } finally {
      setImporting(false);
    }
  };

  const reset = () => {
    setContent("");
    setPreview(null);
    setPreviewError(null);
    setResult(null);
    setPassword("");
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-[720px]">
        <DialogHeader>
          <DialogTitle>{t("codexSession.importTitle", "导入 Codex Session")}</DialogTitle>
          <DialogDescription>
            {t("codexSession.importDescription", "支持 Codex CLI auth.json、CPA token、sub2api 导入包、裸 JWT、JSONL 与 cc-switch envelope。无需走浏览器登录。")}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <Textarea
            value={content}
            onChange={(e) => onContentChange(e.target.value)}
            placeholder={t("codexSession.pastePlaceholder", "粘贴 session JSON / JWT / JSONL ...") as string}
            className="font-mono text-xs min-h-[140px]"
          />

          {previewing && (
            <div className="text-xs text-muted-foreground flex items-center gap-2">
              <Loader2 className="h-3 w-3 animate-spin" /> {t("codexSession.parsing", "解析中...")}
            </div>
          )}
          {previewError && (
            <div className="text-xs text-destructive flex items-center gap-2">
              <AlertTriangle className="h-3 w-3" /> {previewError}
            </div>
          )}
          {preview && !previewing && (
            <>
              {crossMachineWarning && (
                <div className="text-xs flex items-start gap-2 rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-amber-700 dark:text-amber-300">
                  <AlertTriangle className="h-3 w-3 mt-0.5 flex-shrink-0" />
                  <span>{crossMachineWarning}</span>
                </div>
              )}
              <div className="border rounded-md">
              <div className="px-3 py-2 text-xs text-muted-foreground flex items-center justify-between border-b">
                <span>
                  {t("codexSession.detectedFormat", "识别格式")}:{" "}
                  <Badge variant="secondary">{SOURCE_LABELS[preview.sniffedFormat] ?? preview.sniffedFormat}</Badge>
                </span>
                <span>
                  {t("codexSession.totalRows", {
                    defaultValue: "共 {{total}} 条，可入库 {{importable}} 条",
                    total: preview.total,
                    importable: importableCount,
                  })}
                </span>
              </div>
              <ScrollArea className="max-h-[200px]">
                <table className="w-full text-xs">
                  <thead className="text-muted-foreground">
                    <tr>
                      <th className="text-left px-3 py-1">{t("codexSession.colIndex", "#")}</th>
                      <th className="text-left px-3 py-1">{t("codexSession.colAccount", "账号")}</th>
                      <th className="text-left px-3 py-1">{t("codexSession.colSource", "来源")}</th>
                      <th className="text-left px-3 py-1">{t("codexSession.colExp", "过期")}</th>
                      <th className="text-left px-3 py-1">{t("codexSession.colFlags", "标记")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {preview.items.map((item) => (
                      <tr key={item.index} className="border-t">
                        <td className="px-3 py-1">{item.index}</td>
                        <td className="px-3 py-1">
                          {item.error ? (
                            <span className="text-destructive">{item.error}</span>
                          ) : (
                            <span>
                              {item.email ?? item.accountId ?? "(unknown)"}
                            </span>
                          )}
                        </td>
                        <td className="px-3 py-1">{SOURCE_LABELS[item.source] ?? item.source}</td>
                        <td className="px-3 py-1">{formatExp(item.exp)}</td>
                        <td className="px-3 py-1 space-x-1">
                          {!item.hasRefreshToken && (
                            <Badge variant="destructive">{t("codexSession.flagNoRefresh", "无 refresh")}</Badge>
                          )}
                          {item.isExpired && (
                            <Badge variant="destructive">{t("codexSession.flagExpired", "已过期")}</Badge>
                          )}
                          {item.warnings.length > 0 && item.hasRefreshToken && !item.isExpired && (
                            <Badge variant="outline">{t("codexSession.flagWarning", "注意")}</Badge>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </ScrollArea>
            </div>
            </>
          )}

          <div className="grid grid-cols-2 gap-3 pt-2">
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2">
              <Label htmlFor="update-existing" className="text-xs">{t("codexSession.optUpdateExisting", "覆盖已存在账号")}</Label>
              <Switch id="update-existing" checked={updateExisting} onCheckedChange={setUpdateExisting} />
            </div>
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2">
              <Label htmlFor="reject-expired" className="text-xs">{t("codexSession.optRejectExpired", "拒绝已过期 token")}</Label>
              <Switch id="reject-expired" checked={rejectExpired} onCheckedChange={setRejectExpired} />
            </div>
            <div className="flex items-center justify-between rounded-md border bg-muted/30 px-3 py-2 col-span-2">
              <div>
                <Label htmlFor="verify-refresh" className="text-xs">{t("codexSession.optVerifyRefresh", "导入后立即验证续期")}</Label>
                <div className="text-[10px] text-muted-foreground">{t("codexSession.optVerifyRefreshHint", "逐条多一次 OAuth 端点调用，确认 refresh_token 仍有效")}</div>
              </div>
              <Switch id="verify-refresh" checked={verifyRefresh} onCheckedChange={setVerifyRefresh} />
            </div>
            {isEncryptedEnvelope && (
              <div className="col-span-2 space-y-1">
                <Label htmlFor="env-password" className="text-xs">{t("codexSession.envPassword", "envelope 密码")}</Label>
                <Input
                  id="env-password"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={t("codexSession.envPasswordPlaceholder", "输入加密备份的密码") as string}
                />
              </div>
            )}
          </div>

          {result && (
            <div className="text-xs space-y-1 border-t pt-2">
              <div>
                {t("codexSession.resultLabel", "结果")}:{" "}
                <Badge variant="default">{t("codexSession.created", { defaultValue: "新增 {{n}}", n: result.created })}</Badge>{" "}
                <Badge variant="secondary">{t("codexSession.updated", { defaultValue: "更新 {{n}}", n: result.updated })}</Badge>{" "}
                <Badge variant="outline">{t("codexSession.skipped", { defaultValue: "跳过 {{n}}", n: result.skipped })}</Badge>{" "}
                {result.failed > 0 && <Badge variant="destructive">{t("codexSession.failed", { defaultValue: "失败 {{n}}", n: result.failed })}</Badge>}
              </div>
              {result.errors.length > 0 && (
                <ul className="text-destructive">
                  {result.errors.map((err, i) => (
                    <li key={i}>#{err.index} {err.message}</li>
                  ))}
                </ul>
              )}
              {result.warnings.length > 0 && (
                <ul className="text-muted-foreground">
                  {result.warnings.map((w, i) => (
                    <li key={i}>#{w.index} {w.message}</li>
                  ))}
                </ul>
              )}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={importing}>
            {t("codexSession.btnClose", "关闭")}
          </Button>
          <Button
            onClick={handleImport}
            disabled={importing || !content.trim() || importableCount === 0}
          >
            {importing ? (
              <><Loader2 className="mr-2 h-4 w-4 animate-spin" /> {t("codexSession.btnImporting", "导入中")}</>
            ) : (
              <><FileDown className="mr-2 h-4 w-4" /> {t("codexSession.btnImport", { defaultValue: "导入 {{count}} 条", count: importableCount })}</>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
