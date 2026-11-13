import { useMemo, useState } from "react";
import {
  buildComparison,
  describeParams,
  formatBytes,
  isOverDbLimit,
  useHistoryStore,
  type AddressPoint,
  type HistoryResult,
  type TestRunSummary,
} from "../store/history";
import { cn } from "./ui";

function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

function statusText(status: string): string {
  if (status === "ok") return "成功";
  if (status === "timeout") return "超时";
  if (status === "fail") return "失败";
  return "未测";
}

function statusClass(status: string): string {
  if (status === "ok") return "text-text";
  if (status === "timeout") return "text-warn";
  if (status === "fail") return "text-danger";
  return "text-dim";
}

function latencyText(row: HistoryResult): string {
  if (row.status === "timeout") return "超时";
  if (row.status === "fail") return "失败";
  if (row.latencyMs === null) return "—";
  return `${row.latencyMs} ms`;
}

function speedText(row: HistoryResult): string {
  if (row.speedStatus === "fail") return "失败";
  if (row.speedMbps === null) return "—";
  return `${row.speedMbps.toFixed(1)} MB/s`;
}

function Bar({ pct, tone }: { pct: number; tone: "accent" | "warn" }) {
  return (
    <span className="block h-1 w-24 rounded-full bg-panel-2 ring-1 ring-line overflow-hidden">
      <span
        className={cn("block h-full rounded-full", tone === "accent" ? "bg-accent" : "bg-warn")}
        style={{ width: `${Math.min(100, Math.max(0, pct))}%` }}
      />
    </span>
  );
}

function ComparisonTable({ points }: { points: AddressPoint[] }) {
  const rows = useMemo(() => buildComparison(points), [points]);
  if (rows.length === 0) {
    return <div className="px-3 py-2 text-[12px] text-dim">这个地址还没有可对比的历史轮次。</div>;
  }
  return (
    <div className="overflow-auto">
      <table className="w-full min-w-[520px] text-[12px] border-separate border-spacing-0">
        <thead>
          <tr className="text-dim">
            <th className="text-left font-normal px-3 py-1.5 border-b border-line whitespace-nowrap">
              轮次时间
            </th>
            <th className="text-left font-normal px-3 py-1.5 border-b border-line">状态</th>
            <th className="text-right font-normal px-3 py-1.5 border-b border-line">延迟</th>
            <th className="text-left font-normal px-3 py-1.5 border-b border-line">延迟对比</th>
            <th className="text-right font-normal px-3 py-1.5 border-b border-line">速度</th>
            <th className="text-left font-normal px-3 py-1.5 border-b border-line">速度对比</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.runId}>
              <td className="px-3 py-1.5 border-b border-line/60 num text-muted whitespace-nowrap">
                {formatTime(row.startedAt)}
              </td>
              <td className={cn("px-3 py-1.5 border-b border-line/60", statusClass(row.status))}>
                {statusText(row.status)}
              </td>
              <td className="px-3 py-1.5 border-b border-line/60 text-right num text-text">
                {row.latencyMs === null ? "—" : `${row.latencyMs} ms`}
              </td>
              <td className="px-3 py-1.5 border-b border-line/60">
                <Bar pct={row.latencyPct} tone="accent" />
              </td>
              <td className="px-3 py-1.5 border-b border-line/60 text-right num text-text">
                {row.speedMbps === null ? "—" : `${row.speedMbps.toFixed(1)} MB/s`}
              </td>
              <td className="px-3 py-1.5 border-b border-line/60">
                <Bar pct={row.speedPct} tone="warn" />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RunListItem({
  run,
  selected,
  deleting,
  confirming,
  onSelect,
  onAskDelete,
  onDelete,
}: {
  run: TestRunSummary;
  selected: boolean;
  deleting: boolean;
  confirming: boolean;
  onSelect: () => void;
  onAskDelete: () => void;
  onDelete: () => void;
}) {
  return (
    <div
      className={cn(
        "group border-b border-line/70 px-3 py-2 transition-colors",
        selected ? "bg-accent-soft" : "hover:bg-panel-2/70",
      )}
    >
      <button type="button" onClick={onSelect} className="block w-full text-left">
        <div className="flex items-center gap-2">
          <span className="num text-[12px] text-text">{formatTime(run.startedAt)}</span>
          {run.finishedAt === null ? (
            <span className="text-[12px] text-warn">未完成</span>
          ) : null}
        </div>
        <div className="mt-0.5 text-[12px] text-dim truncate" title={describeParams(run)}>
          {describeParams(run)}
        </div>
        <div className="mt-1 flex items-center gap-2 num text-[12px]">
          <span className="text-text">{run.addressCount} 条</span>
          <span className="text-dim">·</span>
          <span className="text-text">成功 {run.ok}</span>
          <span className="text-warn">超时 {run.timeout}</span>
          <span className="text-danger">失败 {run.fail}</span>
        </div>
      </button>
      <div className="mt-1 flex justify-end">
        {confirming ? (
          <button
            type="button"
            onClick={onDelete}
            disabled={deleting}
            className="h-5 px-1.5 border border-danger/50 rounded-[2px] text-[12px] text-danger hover:bg-danger-soft disabled:opacity-50"
          >
            {deleting ? "删除中…" : "确认删除整轮"}
          </button>
        ) : (
          <button
            type="button"
            onClick={onAskDelete}
            className="h-5 px-1.5 border border-line rounded-[2px] text-[12px] text-dim opacity-0 group-hover:opacity-100 hover:text-danger hover:border-danger/50"
          >
            删除
          </button>
        )}
      </div>
    </div>
  );
}

export default function HistoryPanel() {
  const open = useHistoryStore((state) => state.open);
  const loading = useHistoryStore((state) => state.loading);
  const runs = useHistoryStore((state) => state.runs);
  const selectedRunId = useHistoryStore((state) => state.selectedRunId);
  const detail = useHistoryStore((state) => state.detail);
  const detailLoading = useHistoryStore((state) => state.detailLoading);
  const address = useHistoryStore((state) => state.address);
  const addressLoading = useHistoryStore((state) => state.addressLoading);
  const dbBytes = useHistoryStore((state) => state.dbBytes);
  const deleting = useHistoryStore((state) => state.deleting);
  const error = useHistoryStore((state) => state.error);
  const closePanel = useHistoryStore((state) => state.closePanel);
  const refresh = useHistoryStore((state) => state.refresh);
  const selectRun = useHistoryStore((state) => state.selectRun);
  const deleteRun = useHistoryStore((state) => state.deleteRun);
  const loadAddress = useHistoryStore((state) => state.loadAddress);
  const clearAddress = useHistoryStore((state) => state.clearAddress);
  const [confirmId, setConfirmId] = useState<string | null>(null);

  const overLimit = isOverDbLimit(dbBytes);

  if (!open) return null;

  const selectedRun = runs.find((run) => run.id === selectedRunId) ?? null;
  const runKind = selectedRun ? String(selectedRun.params.kind ?? "") : "";
  const runTransport = selectedRun ? String(selectedRun.params.transport ?? "") : "";
  const runTls =
    selectedRun && typeof selectedRun.params.tls === "boolean"
      ? selectedRun.params.tls
        ? "TLS"
        : "无"
      : "";

  return (
    <div
      className="fixed inset-0 z-40 bg-black/25 flex items-center justify-center p-6"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) closePanel();
      }}
    >
      <section
        role="dialog"
        aria-label="历史结果"
        className="w-full max-w-[1120px] h-[78vh] min-h-[420px] flex flex-col border border-line-2 rounded-[3px] bg-panel shadow-[0_16px_48px_rgba(23,23,23,0.18)]"
      >
        <header className="h-11 shrink-0 flex items-center gap-3 px-4 border-b border-line bg-panel-2">
          <h2 className="text-[14px] font-medium text-text">历史结果</h2>
          <span className="text-[12px] text-dim">按测试轮次回看，结果全量保留</span>
          <span
            className={cn(
              "ml-auto inline-flex items-center gap-1.5 h-6 px-2 border rounded-[3px] text-[12px] num",
              overLimit
                ? "border-warn/50 text-warn bg-warn/[0.06]"
                : "border-line text-muted bg-panel",
            )}
            title="数据库体积"
          >
            数据库 {formatBytes(dbBytes)}
          </span>
          {overLimit ? (
            <span className="text-[12px] text-warn">已超过 50MB，请按轮次清理</span>
          ) : null}
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={loading}
            className="h-6 px-2 border border-line rounded-[3px] text-[12px] text-muted hover:text-accent hover:border-accent-dim disabled:opacity-50"
          >
            {loading ? "刷新中…" : "刷新"}
          </button>
          <button
            type="button"
            onClick={closePanel}
            aria-label="关闭历史"
            className="size-6 grid place-items-center border border-line rounded-[3px] text-dim hover:text-text"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M6 6l12 12M18 6 6 18" />
            </svg>
          </button>
        </header>

        {error ? (
          <div className="shrink-0 px-4 py-1.5 border-b border-line bg-danger/[0.06] text-[12px] text-danger">
            {error}
          </div>
        ) : null}

        <div className="flex-1 min-h-0 flex">
          <aside className="w-[300px] shrink-0 border-r border-line flex flex-col min-h-0">
            <div className="h-8 shrink-0 flex items-center justify-between px-3 border-b border-line text-[12px] text-dim">
              <span>共 {runs.length} 轮</span>
              <span>点击查看结果</span>
            </div>
            <div className="flex-1 min-h-0 overflow-auto">
              {runs.length === 0 ? (
                <div className="px-3 py-8 text-center text-[12px] text-dim">
                  {loading ? "读取中…" : "还没有测试轮次"}
                </div>
              ) : (
                runs.map((run) => (
                  <RunListItem
                    key={run.id}
                    run={run}
                    selected={run.id === selectedRunId}
                    deleting={deleting === run.id}
                    confirming={confirmId === run.id}
                    onSelect={() => {
                      setConfirmId(null);
                      void selectRun(run.id);
                    }}
                    onAskDelete={() => setConfirmId(run.id)}
                    onDelete={() => {
                      setConfirmId(null);
                      void deleteRun(run.id);
                    }}
                  />
                ))
              )}
            </div>
          </aside>

          <div className="flex-1 min-w-0 flex flex-col min-h-0">
            {selectedRun === null ? (
              <div className="flex-1 grid place-items-center text-[12px] text-dim">
                从左侧选择一轮测试查看全部结果
              </div>
            ) : detailLoading ? (
              <div className="flex-1 grid place-items-center text-[12px] text-dim">读取结果中…</div>
            ) : (
              <>
                <div className="shrink-0 px-4 py-2 border-b border-line flex flex-wrap items-center gap-x-4 gap-y-1 text-[12px]">
                  <span className="num text-text">{formatTime(selectedRun.startedAt)}</span>
                  <span className="text-dim">{describeParams(selectedRun)}</span>
                  <span className="num text-muted">
                    成功 <span className="text-text">{selectedRun.ok}</span> · 超时{" "}
                    <span className="text-warn">{selectedRun.timeout}</span> · 失败{" "}
                    <span className="text-danger">{selectedRun.fail}</span> · 地址{" "}
                    <span className="text-text">{selectedRun.addressCount}</span> 条
                  </span>
                  <span className="ml-auto text-dim">只读 · 点击行查看该地址跨轮对比</span>
                </div>

                <div className="flex-1 min-h-0 overflow-auto">
                  <table className="w-full min-w-[1040px] text-[12px] border-separate border-spacing-0">
                    <thead>
                      <tr className="text-dim">
                        {[
                          "类型",
                          "别名",
                          "地址",
                          "端口",
                          "传输",
                          "归属地(中)",
                          "归属地(英)",
                          "机房",
                          "TLS",
                          "延迟",
                          "速度",
                          "状态",
                        ].map((label) => (
                          <th
                            key={label}
                            className={cn(
                              "sticky top-0 z-10 bg-panel-2 border-b border-line font-normal px-2 h-8 whitespace-nowrap",
                              label === "端口" || label === "延迟" || label === "速度" || label === "机房" || label === "TLS"
                                ? "text-right"
                                : "text-left",
                            )}
                          >
                            {label}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {(detail?.results ?? []).map((row) => {
                        const active =
                          address?.host === row.host && address?.port === row.port;
                        return (
                          <tr
                            key={`${row.host}:${row.port}`}
                            onClick={() => void loadAddress(row.host, row.port)}
                            className={cn(
                              "cursor-pointer transition-colors",
                              active ? "bg-accent-soft" : "hover:bg-panel-2/70",
                            )}
                            title="查看该地址跨轮对比"
                          >
                            <td className="border-b border-line/60 px-2 py-1.5 text-muted">
                              {runKind || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 max-w-[200px] truncate text-text/85">
                              {row.alias || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 num text-text">
                              {row.host}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 num text-right text-muted">
                              {row.port}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 num text-muted">
                              {runTransport || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5">
                              {row.zh || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 text-muted">
                              {row.en || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 num text-right">
                              {row.colo || <span className="text-dim">—</span>}
                            </td>
                            <td className="border-b border-line/60 px-2 py-1.5 text-right text-muted">
                              {runTls || <span className="text-dim">—</span>}
                            </td>
                            <td
                              className={cn(
                                "border-b border-line/60 px-2 py-1.5 num text-right",
                                statusClass(row.status),
                              )}
                              title={row.error ?? undefined}
                            >
                              {latencyText(row)}
                            </td>
                            <td
                              className="border-b border-line/60 px-2 py-1.5 num text-right text-text/90"
                              title={row.speedError ?? undefined}
                            >
                              {speedText(row)}
                            </td>
                            <td className={cn("border-b border-line/60 px-2 py-1.5", statusClass(row.status))}>
                              {statusText(row.status)}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                  {(detail?.results ?? []).length === 0 ? (
                    <div className="py-10 text-center text-[12px] text-dim">这一轮没有结果行</div>
                  ) : null}
                </div>

                <div className="shrink-0 border-t border-line bg-panel-2/60 max-h-[40%] flex flex-col min-h-0">
                  <div className="h-8 shrink-0 flex items-center gap-2 px-4 text-[12px]">
                    <span className="text-text">跨轮对比</span>
                    {address ? (
                      <>
                        <span className="num text-muted">
                          {address.host}:{address.port}
                        </span>
                        <span className="text-dim">按时间升序</span>
                        <button
                          type="button"
                          onClick={clearAddress}
                          className="ml-auto text-dim hover:text-text"
                        >
                          收起
                        </button>
                      </>
                    ) : (
                      <span className="text-dim">点击上方任一行，查看该地址各轮的延迟与速度</span>
                    )}
                  </div>
                  {address ? (
                    <div className="flex-1 min-h-0 overflow-auto border-t border-line/70">
                      {addressLoading ? (
                        <div className="px-3 py-2 text-[12px] text-dim">读取中…</div>
                      ) : (
                        <ComparisonTable points={address.points} />
                      )}
                    </div>
                  ) : null}
                </div>
              </>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}
