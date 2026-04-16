import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useRequestLogs } from "@/lib/query/usage";
import { formatUtcDateTime } from "@/utils/shareUtils";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

interface ShareRequestLogTableProps {
  shareId: string;
  active?: boolean;
}

export function ShareRequestLogTable({
  shareId,
  active = true,
}: ShareRequestLogTableProps) {
  const { t } = useTranslation();
  const [page, setPage] = useState(0);
  const pageSize = 10;

  useEffect(() => {
    setPage(0);
  }, [shareId]);

  const { data, isLoading } = useRequestLogs({
    filters: { shareId },
    range: { preset: "7d" },
    page,
    pageSize,
    options: {
      refetchInterval: active ? 10000 : false,
      refetchIntervalInBackground: active,
    },
  });

  const logs = data?.data ?? [];
  const total = data?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const canGoPrev = page > 0;
  const canGoNext = page + 1 < totalPages;
  const rangeLabel = useMemo(() => {
    if (total === 0) {
      return t("share.requestLogsRangeEmpty");
    }
    const start = page * pageSize + 1;
    const end = Math.min(total, start + logs.length - 1);
    return t("share.requestLogsRange", {
      start,
      end,
      total,
    });
  }, [logs.length, page, pageSize, t, total]);

  return (
    <section className="space-y-3">
      <div className="flex items-center justify-between">
        <div className="text-sm font-medium">{t("share.requestLogs")}</div>
        <div className="text-xs text-muted-foreground">
          {t("share.requestLogsHint")}
        </div>
      </div>
      <Card className="border-border-default/70 bg-muted/10">
        <CardContent className="px-0 py-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t("share.requestTime")}</TableHead>
                <TableHead>{t("share.requestModel")}</TableHead>
                <TableHead>{t("share.requestInput")}</TableHead>
                <TableHead>{t("share.requestOutput")}</TableHead>
                <TableHead>{t("share.requestCacheRead")}</TableHead>
                <TableHead>{t("share.requestCacheCreate")}</TableHead>
                <TableHead>{t("share.requestTotal")}</TableHead>
                <TableHead>{t("share.requestStatus")}</TableHead>
                <TableHead>{t("share.requestLatency")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {isLoading ? (
                <TableRow>
                  <TableCell colSpan={9} className="h-24 text-center text-muted-foreground">
                    {t("common.loading")}
                  </TableCell>
                </TableRow>
              ) : logs.length ? (
                logs.map((log) => (
                  <TableRow key={log.requestId}>
                    <TableCell className="whitespace-nowrap">
                      {formatUtcDateTime(log.createdAt * 1000)}
                    </TableCell>
                    <TableCell>
                      <div className="font-medium">{log.model || "-"}</div>
                      <div className="text-xs text-muted-foreground">
                        {log.providerName || log.appType}
                      </div>
                    </TableCell>
                    <TableCell>{log.inputTokens}</TableCell>
                    <TableCell>{log.outputTokens}</TableCell>
                    <TableCell>{log.cacheReadTokens}</TableCell>
                    <TableCell>{log.cacheCreationTokens}</TableCell>
                    <TableCell>
                      {log.inputTokens +
                        log.outputTokens +
                        log.cacheReadTokens +
                        log.cacheCreationTokens}
                    </TableCell>
                    <TableCell>{log.statusCode}</TableCell>
                    <TableCell>{log.latencyMs} ms</TableCell>
                  </TableRow>
                ))
              ) : (
                <TableRow>
                  <TableCell colSpan={9} className="h-24 text-center text-muted-foreground">
                    {t("share.requestLogsEmpty")}
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
      <div className="flex flex-col gap-2 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
        <div>{rangeLabel}</div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={isLoading || !canGoPrev}
            onClick={() => setPage((current) => Math.max(0, current - 1))}
          >
            {t("share.requestLogsPrev")}
          </Button>
          <div>
            {t("share.requestLogsPage", {
              page: page + 1,
              totalPages,
            })}
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={isLoading || !canGoNext}
            onClick={() => setPage((current) => current + 1)}
          >
            {t("share.requestLogsNext")}
          </Button>
        </div>
      </div>
    </section>
  );
}
