import { useTranslation } from "react-i18next";
import { Copy } from "lucide-react";
import type { ShareRecord, TunnelConfig } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { useShareConnectInfoQuery } from "@/lib/query";
import { buildShareCurlExample } from "@/utils/shareUtils";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { resolveShareTunnelInfo } from "@/utils/shareUtils";

interface ShareConnectDialogProps {
  share: ShareRecord | null;
  tunnelConfig: TunnelConfig;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function ShareConnectDialog({
  share,
  tunnelConfig,
  open,
  onOpenChange,
}: ShareConnectDialogProps) {
  const { t } = useTranslation();
  const { data, isLoading } = useShareConnectInfoQuery(share?.id, open);
  const tunnelDisplay = share
    ? resolveShareTunnelInfo(share, tunnelConfig)
    : null;

  const handleCopy = async (value: string, key: string) => {
    await copyText(value);
    toast.success(t(key));
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("share.connectInfo")}</DialogTitle>
          <DialogDescription>{t("share.connectDescription")}</DialogDescription>
        </DialogHeader>

        {isLoading || !share || !data ? (
          <div className="px-6 pb-6 pt-4 text-center text-sm text-muted-foreground">
            {t("common.loading")}
          </div>
        ) : (
          <div className="space-y-4 px-6 pb-6 pt-4">
            <ConnectRow
              label={t("share.tunnelUrl")}
              value={data.tunnelUrl || tunnelDisplay?.tunnelUrl || ""}
              copyLabel={t("share.copyUrl")}
              onCopy={() =>
                void handleCopy(
                  data.tunnelUrl || tunnelDisplay?.tunnelUrl || "",
                  "share.toast.copyUrl",
                )
              }
            />
            <ConnectRow
              label={t("share.apiKey")}
              value={data.apiKey}
              copyLabel={t("share.copyApiKey")}
              onCopy={() =>
                void handleCopy(data.apiKey, "share.toast.copyApiKey")
              }
            />
            <ConnectRow
              label={t("share.subdomain")}
              value={data.subdomain || tunnelDisplay?.subdomain || ""}
              copyLabel={t("share.copySubdomain")}
              onCopy={() =>
                void handleCopy(
                  data.subdomain || tunnelDisplay?.subdomain || "",
                  "share.toast.copySubdomain",
                )
              }
            />
            <div className="space-y-2">
              <div className="text-sm font-medium">{t("share.copyCurl")}</div>
              <pre className="overflow-x-auto rounded-xl border border-border-default bg-muted/30 p-4 text-xs">
                {buildShareCurlExample(data)}
              </pre>
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  void handleCopy(
                    buildShareCurlExample(data),
                    "share.toast.copyCurl",
                  )
                }
              >
                <Copy className="h-4 w-4" />
                {t("share.copyCurl")}
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function ConnectRow({
  label,
  value,
  copyLabel,
  onCopy,
}: {
  label: string;
  value: string;
  copyLabel: string;
  onCopy: () => void;
}) {
  return (
    <div className="rounded-xl border border-border-default bg-muted/20 px-4 py-3">
      <div className="text-xs uppercase tracking-[0.14em] text-muted-foreground">
        {label}
      </div>
      <div className="mt-2 flex items-start justify-between gap-3">
        <code className="break-all text-sm">{value || "-"}</code>
        <Button variant="outline" size="sm" onClick={onCopy}>
          <Copy className="h-4 w-4" />
          {copyLabel}
        </Button>
      </div>
    </div>
  );
}
