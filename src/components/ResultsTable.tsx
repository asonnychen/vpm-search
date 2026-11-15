import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePoolStore } from "../store/pool";
import {
  probeKey,
  selectProbeError,
  selectProbeGeos,
  selectProbeResults,
  selectProbeSpeeds,
  selectProbeTesting,
  useProbeStore,
  type GeoResult,
} from "../store/probe";
import { selectColumnWidths, selectThresholdMs, useSettingsStore } from "../store/settings";
import { useSelectionStore } from "../store/selection";
import { sortRows, type SortDir, type SortKey } from "../store/tableSort";
import { countMissingGeo, pickExportRange, type ExportRow } from "../store/exportRows";
import { exportTxt } from "../store/exportFile";
import { cn } from "./ui";

type RowStatus = "untested" | "ok" | "timeout" | "fail";

/** 表格行来自优选域名池，测量值来自探测结果。 */
type Row = {
  id: string;
  host: string;
  port: number;
  alias: string;
  source: string;
  latency: number | null;
  speed: number | null;
  /** 测速状态，与延迟状态分开，失败时速度列显示「失败」。 */
  speedStatus: "ok" | "fail" | "untested";
  status: RowStatus;
  /** 失败或超时的原因，悬浮时显示。 */
  error: string | null;
  /** 测速失败原因。 */
  speedError: string | null;
  /** 地址本身的归属地；未查询或查询失败时为 null。 */
  geo: GeoResult | null;
};

const filterDefs = [
  { id: "all", label: "全部" },
  { id: "ok", label: "成功" },
  { id: "timeout", label: "超时" },
  { id: "fail", label: "失败" },
  { id: "slow", label: "超阈值" },
] as const;

type FilterId = (typeof filterDefs)[number]["id"];

/** 行高固定 32px：虚拟滚动按这个值算位置，表格行必须严格一致。 */
const ROW_HEIGHT = 32;
/** 视口外多渲染几行，滚动时不出现空白。 */
const OVERSCAN = 12;

function latencyCell(row: Row, threshold: number) {
  if (row.status === "timeout") {
    return <span className="text-warn text-[12px]">超时</span>;
  }
  if (row.status === "fail") {
    return <span className="text-danger text-[12px]">失败</span>;
  }
  if (row.latency === null) {
    return <span className="text-dim">—</span>;
  }
  const over = row.latency > threshold;
  return (
    <span className="inline-flex items-center justify-end gap-1.5">
      <span className="w-10 h-1 bg-panel-2 ring-1 ring-line rounded-full overflow-hidden">
        <span
          className={cn("block h-full rounded-full", over ? "bg-danger" : "bg-accent")}
          style={{ width: `${Math.min(100, (row.latency / threshold) * 100)}%` }}
        />
      </span>
      <span className={cn("num", over ? "text-danger" : "text-text")}>
        {row.latency}
        <span className="text-dim text-[12px] ml-0.5">ms</span>
      </span>
    </span>
  );
}

function geoTitle(geo: GeoResult | null): string | undefined {
  if (!geo) return undefined;
  const parts = [
    geo.ip ? `IP ${geo.ip}` : "",
    geo.country ? `国家码 ${geo.country}` : "",
    geo.source ? `来源 ${geo.source}` : "",
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : undefined;
}

export default function ResultsTable() {
  const [filter, setFilter] = useState<FilterId>("all");
  const [query, setQuery] = useState("");
  const selected = useSelectionStore((state) => state.ids);
  const setIds = useSelectionStore((state) => state.setIds);
  const setSelected = (updater: string[] | ((prev: string[]) => string[])) =>
    setIds(typeof updater === "function" ? updater(selected) : updater);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({
    key: "latency",
    dir: "asc",
  });
  const [exporting, setExporting] = useState(false);
  const [exportNotice, setExportNotice] = useState<{
    tone: "ok" | "warn" | "err";
    text: string;
  } | null>(null);

  const poolEntries = usePoolStore((state) => state.entries);
  // 单条真连接延迟的结果按 `host:port` 覆盖到对应行，未测的行保持 null。
  const probeResults = useProbeStore(selectProbeResults);
  const probeSpeeds = useProbeStore(selectProbeSpeeds);
  const probeGeos = useProbeStore(selectProbeGeos);
  const probeTesting = useProbeStore(selectProbeTesting);
  const probeError = useProbeStore(selectProbeError);
  const testOne = useProbeStore((state) => state.testOne);

  const rows = useMemo<Row[]>(
    () =>
      poolEntries.map((entry) => {
        const key = probeKey(entry.host, entry.port);
        const result = probeResults[key];
        const speed = probeSpeeds[key];
        return {
          id: key,
          host: entry.host,
          port: entry.port,
          alias: entry.alias,
          source: entry.source,
          latency: result?.latencyMs ?? null,
          speed: speed?.mbps ?? null,
          speedStatus: speed?.status ?? "untested",
          status: result?.status ?? "untested",
          error: result?.error ?? null,
          speedError: speed?.error ?? null,
          geo: probeGeos[key] ?? null,
        };
      }),
    [poolEntries, probeResults, probeSpeeds, probeGeos],
  );

  const counts = useMemo(() => {
    const base = { all: rows.length, ok: 0, timeout: 0, fail: 0, slow: 0 };
    for (const row of rows) {
      if (row.status === "ok") base.ok += 1;
      if (row.status === "timeout") base.timeout += 1;
      if (row.status === "fail") base.fail += 1;
    }
    return base;
  }, [rows]);

  const toggleSort = (key: SortKey) =>
    setSort((s) =>
      s.key === key ? { key, dir: s.dir === "asc" ? "desc" : "asc" } : { key, dir: "asc" },
    );

  // 列宽来自设置 store：拖拽后防抖落库，重启还原。
  const widths = useSettingsStore(selectColumnWidths);
  const setColumnWidth = useSettingsStore((state) => state.setColumnWidth);
  const threshold = useSettingsStore(selectThresholdMs);
  const latencyFilterMs = useSettingsStore((state) => state.settings.ui.latencyFilterMs);
  const patchSettings = useSettingsStore((state) => state.patch);
  const resizeRef = useRef<{ i: number; x: number; w: number } | null>(null);

  useEffect(() => {
    const move = (e: MouseEvent) => {
      const r = resizeRef.current;
      if (!r) return;
      setColumnWidth(r.i, Math.max(48, r.w + (e.clientX - r.x)));
    };
    const up = () => {
      resizeRef.current = null;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, [setColumnWidth]);

  const startResize = (i: number) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    resizeRef.current = { i, x: e.clientX, w: widths[i] };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  // 排序作用于全表，再按筛选出可见行；导出顺序也跟这份排序一致。
  // 空值（超时 / 失败 / 未测）始终排在末尾，不参与升降序翻转。
  const sortedRows = useMemo(
    () => sortRows(rows, sort.key, sort.dir),
    [rows, sort],
  );

  const shown = useMemo(() => {
    return sortedRows.filter((r) => {
      if (filter === "ok" && r.status !== "ok") return false;
      if (filter === "timeout" && r.status !== "timeout") return false;
      if (filter === "fail" && r.status !== "fail") return false;
      if (filter === "slow" && !(r.latency !== null && r.latency > threshold)) return false;
      // 延时上限过滤：未测的行没有延时，过滤开启时一并隐藏。
      if (latencyFilterMs !== null && !(r.latency !== null && r.latency <= latencyFilterMs)) {
        return false;
      }
      if (query) {
        const q = query.toLowerCase();
        return (
          r.host.toLowerCase().includes(q) ||
          r.alias.toLowerCase().includes(q) ||
          r.source.toLowerCase().includes(q) ||
          (r.geo?.zh.toLowerCase().includes(q) ?? false) ||
          (r.geo?.en.toLowerCase().includes(q) ?? false)
        );
      }
      return true;
    });
  }, [filter, query, threshold, latencyFilterMs, sortedRows]);

  const toggle = (id: string) =>
    setSelected((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]));

  const shownIds = useMemo(() => shown.map((r) => r.id), [shown]);
  // 勾选集合查表用 Set，避免每行都对数组做一次 includes。
  const selectedSet = useMemo(() => new Set(selected), [selected]);

  // 导出用全表快照，顺序即当前排序；勾选非空时范围就是勾选行。
  const exportRows = useMemo<ExportRow[]>(
    () =>
      sortedRows.map((r) => ({
        id: r.id,
        host: r.host,
        port: r.port,
        alias: r.alias,
        source: r.source,
        geo: r.geo,
      })),
    [sortedRows],
  );
  const exportRange = useMemo(
    () => pickExportRange(exportRows, selected, shownIds),
    [exportRows, selected, shownIds],
  );
  const exportMissing = useMemo(
    () => countMissingGeo(exportRange),
    [exportRange],
  );

  const handleExport = async () => {
    if (exportRange.length === 0) {
      setExportNotice({ tone: "warn", text: "没有可导出的地址" });
      return;
    }
    setExporting(true);
    setExportNotice(null);
    try {
      const summary = await exportTxt(exportRows, selected, shownIds);
      // 取消不提示，避免误以为出了错。
      if (!summary.cancelled) {
        setExportNotice({
          tone: summary.missingGeo > 0 ? "warn" : "ok",
          text: `已导出 ${summary.rows} 条到 ${summary.path ?? ""}${
            summary.missingGeo > 0
              ? `，其中 ${summary.missingGeo} 条缺少归属地`
              : ""
          }`,
        });
      }
    } catch (error) {
      setExportNotice({
        tone: "err",
        text: error instanceof Error ? error.message : String(error),
      });
    } finally {
      setExporting(false);
    }
  };

  const allShown = useMemo(
    () => shownIds.length > 0 && shownIds.every((id) => selectedSet.has(id)),
    [shownIds, selectedSet],
  );
  const someShown = useMemo(
    () => shownIds.some((id) => selectedSet.has(id)),
    [shownIds, selectedSet],
  );

  const toggleAll = () =>
    setSelected((s) =>
      allShown
        ? s.filter((id) => !shownIds.includes(id))
        : Array.from(new Set([...s, ...shownIds])),
    );

  const invert = () =>
    setSelected((s) => [
      ...s.filter((id) => !shownIds.includes(id)),
      ...shownIds.filter((id) => !s.includes(id)),
    ]);

  // 池子可能上千行，只渲染视口内的行：行高固定 32px，位置由虚拟滚动算。
  // 表头 sticky、横向滚动、列宽拖拽都不受影响；全选/反选仍按 shownIds（筛选后的全部行）。
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const virtualizer = useVirtualizer({
    count: shown.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: OVERSCAN,
    getItemKey: (index) => shown[index]?.id ?? index,
  });
  const virtualRows = virtualizer.getVirtualItems();
  const padTop = virtualRows.length > 0 ? virtualRows[0].start : 0;
  const padBottom =
    virtualRows.length > 0
      ? Math.max(
          0,
          virtualizer.getTotalSize() - virtualRows[virtualRows.length - 1].end,
        )
      : 0;

  // 换了筛选条件就回到顶部，避免停在已经不存在的行号上。
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [filter, query]);

  return (
    <section className="flex-1 min-h-0 flex flex-col overflow-hidden bg-panel">
      <div className="min-h-10 shrink-0 flex flex-wrap items-center gap-x-2 gap-y-1.5 px-4 py-1.5 border-b border-line bg-panel">
        <div className="flex items-center gap-0.5 border border-line rounded-[3px] p-0.5 bg-canvas/60">
          {filterDefs.map((f) => (
            <button
              key={f.id}
              type="button"
              onClick={() => setFilter(f.id)}
              className={cn(
                "h-6 px-2 rounded-[2px] text-[12px] flex items-center gap-1 transition-colors whitespace-nowrap",
                filter === f.id
                  ? "bg-accent/[0.12] text-accent"
                  : "text-muted hover:text-text",
              )}
            >
              {f.label}
              <span
                className={cn(
                  "num text-[12px]",
                  filter === f.id ? "text-accent/70" : "text-dim",
                )}
              >
                {counts[f.id]}
              </span>
            </button>
          ))}
        </div>

        <div className="relative ml-1">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索地址 / 别名 / 归属地"
            className="h-7 w-56 bg-canvas/60 border border-line rounded-[3px] pl-7 pr-2 text-[12px] outline-none focus:border-accent-dim placeholder:text-dim/80"
          />
          <svg
            className="absolute left-2 top-1/2 -translate-y-1/2 text-dim"
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
          >
            <circle cx="11" cy="11" r="7" />
            <path d="m20 20-3.5-3.5" />
          </svg>
        </div>

        <div className="ml-auto flex items-center gap-3 text-[12px] text-dim num">
          {exportMissing > 0 ? (
            <span className="text-warn">
              {exportMissing} 条缺少归属地，导出时该段留空
            </span>
          ) : null}
          <label
            className="flex items-center gap-1 whitespace-nowrap"
            title="只看延时不超过该值的地址；清空表示不限"
          >
            <span>延时 ≤</span>
            <input
              value={latencyFilterMs ?? ""}
              onChange={(e) => {
                const digits = e.target.value.replace(/[^\d]/g, "");
                patchSettings({
                  ui: { latencyFilterMs: digits === "" ? null : Number(digits) },
                });
              }}
              inputMode="numeric"
              placeholder="不限"
              aria-label="延时过滤上限（毫秒）"
              className="h-7 w-16 border border-line rounded-[3px] bg-panel px-1.5 text-right text-[12px] text-text outline-none focus:border-accent-dim placeholder:text-dim/70"
            />
            <span>ms</span>
            {latencyFilterMs !== null ? (
              <button
                type="button"
                onClick={() => patchSettings({ ui: { latencyFilterMs: null } })}
                title="清除延时过滤"
                className="text-dim hover:text-text px-0.5"
              >
                ×
              </button>
            ) : null}
          </label>
          <button
            type="button"
            onClick={() => void handleExport()}
            disabled={exporting || rows.length === 0}
            title={selected.length > 0 ? "导出勾选行" : "导出当前筛选后的可见行"}
            className={cn(
              "h-7 px-2.5 border rounded-[3px] text-[12px] whitespace-nowrap",
              exporting || rows.length === 0
                ? "border-line text-dim cursor-not-allowed"
                : "border-accent bg-accent text-white hover:bg-accent-strong",
            )}
          >
            {exporting ? "导出中…" : "导出 txt"}
          </button>
          <span>
            共 <span className="text-text">{rows.length}</span> 条
          </span>
          {latencyFilterMs !== null ? (
            <span>
              筛后 <span className="text-text">{shown.length}</span> 条
            </span>
          ) : null}
          <span>
            未测 <span className="text-text">{rows.length - counts.ok - counts.timeout - counts.fail}</span>
          </span>
          <span className="text-line-2">|</span>
          <span>
            已选 <span className="text-text">{selected.length}</span>
          </span>
        </div>
      </div>

      {probeError ? (
        <div className="shrink-0 flex items-start gap-2 px-4 py-1.5 border-b border-line bg-danger/[0.06] text-[12px] text-danger">
          <span className="font-medium">测试失败</span>
          <span className="num break-all opacity-90">{probeError}</span>
        </div>
      ) : null}

      {exportNotice ? (
        <div
          className={cn(
            "shrink-0 flex items-start gap-2 px-4 py-1.5 border-b border-line text-[12px]",
            exportNotice.tone === "err"
              ? "bg-danger/[0.06] text-danger"
              : exportNotice.tone === "warn"
                ? "bg-warn/[0.06] text-warn"
                : "bg-accent-soft text-accent",
          )}
        >
          <span className="font-medium">导出</span>
          <span className="break-all opacity-90">{exportNotice.text}</span>
        </div>
      ) : null}

      <div
        ref={scrollRef}
        className="flex-1 min-h-0 overflow-auto border border-line rounded-[3px] bg-panel"
      >
        <table className="w-full min-w-[1180px] table-fixed border-separate border-spacing-0 text-[12px] [&_td]:overflow-hidden [&_th]:border-r [&_td]:border-r [&_th:last-child]:border-r-0 [&_td:last-child]:border-r-0">
          <colgroup>
            {widths.map((w, i) => (
              <col key={i} style={{ width: w }} />
            ))}
          </colgroup>
          <thead>
            <tr className="text-[12px]  text-dim">
              <th className="relative sticky top-0 z-10 bg-panel-2 border-b border-line font-normal text-left px-2 h-8 whitespace-nowrap">
                <span
                  onMouseDown={startResize(0)}
                  className="absolute right-0 top-0 h-full w-[5px] cursor-col-resize hover:bg-accent/30"
                />
                <div className="flex items-center gap-1.5">
                  <button
                    type="button"
                    role="checkbox"
                    aria-checked={allShown ? true : someShown ? "mixed" : false}
                    aria-label="全选 / 取消全选"
                    title="全选 / 取消全选"
                    onClick={toggleAll}
                    className={cn(
                      "size-3.5 grid place-items-center border rounded-[2px] leading-none",
                      allShown || someShown
                        ? "border-accent bg-accent text-white"
                        : "border-line-2 text-transparent hover:border-muted",
                    )}
                  >
                    {allShown ? (
                      <svg width="10" height="10" viewBox="0 0 12 12" fill="none" aria-hidden>
                        <path
                          d="M2.6 6.3 4.7 8.4 9.4 3.6"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        />
                      </svg>
                    ) : someShown ? (
                      <span className="block w-[7px] h-[2px] rounded-full bg-white" />
                    ) : null}
                  </button>
                  <button
                    type="button"
                    onClick={invert}
                    title="反选"
                    aria-label="反选"
                    className="text-[12px] normal-case tracking-normal text-dim hover:text-accent leading-none"
                  >
                    反选
                  </button>
                </div>
              </th>
              {[
                { label: "别名" },
                { label: "地址" },
                { label: "端口", align: "right" },
                { label: "归属地(中)" },
                { label: "归属地(英)" },
                { label: "优选域名" },
                { label: "延迟", align: "right", sortKey: "latency" as const },
                { label: "速度", align: "right", sortKey: "speed" as const },
                { label: "操作", align: "right" },
              ].map((col, idx) => (
                <th
                  key={col.label}
                  className={cn(
                    "relative sticky top-0 z-10 bg-panel-2 border-b border-line font-normal text-left px-2 h-8 whitespace-nowrap",
                    col.align === "right" ? "text-right" : "",
                  )}
                >
                  <span
                    onMouseDown={startResize(idx + 1)}
                    className="absolute right-0 top-0 h-full w-[5px] cursor-col-resize hover:bg-accent/30"
                  />
                  {col.sortKey ? (
                    <button
                      type="button"
                      onClick={() => toggleSort(col.sortKey!)}
                      title={`按${col.label}排序`}
                      aria-sort={
                        sort.key === col.sortKey
                          ? sort.dir === "asc"
                            ? "ascending"
                            : "descending"
                          : "none"
                      }
                      className={cn(
                        "inline-flex w-full items-center justify-end gap-1 hover:text-text",
                        sort.key === col.sortKey ? "text-text" : "text-dim",
                      )}
                    >
                      {col.label}
                      <span className="w-2 text-[12px] leading-none">
                        {sort.key === col.sortKey
                          ? sort.dir === "asc"
                            ? "↑"
                            : "↓"
                          : ""}
                      </span>
                    </button>
                  ) : (
                    col.label
                  )}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {padTop > 0 ? (
              <tr aria-hidden style={{ height: padTop }}>
                <td colSpan={widths.length} style={{ padding: 0, border: 0 }} />
              </tr>
            ) : null}
            {virtualRows.map((virtualRow) => {
              const r = shown[virtualRow.index];
              if (!r) return null;
              const isSel = selectedSet.has(r.id);
              const slow = r.latency !== null && r.latency > threshold;
              const busy = Boolean(probeTesting[r.id]);
              return (
                <tr
                  key={r.id}
                  style={{ height: ROW_HEIGHT }}
                  className={cn(
                    "group transition-colors",
                    isSel ? "bg-accent/[0.07]" : "hover:bg-panel-2/70",
                  )}
                >
                  <td className="border-b border-line/60 px-2 h-8 w-8">
                    <button
                      type="button"
                      onClick={() => toggle(r.id)}
                      className={cn(
                        "size-3.5 grid place-items-center border rounded-[2px]",
                        isSel
                          ? "border-accent bg-accent text-white"
                          : "border-line-2 text-transparent hover:border-muted",
                      )}
                      aria-label="选择"
                    >
                      <svg width="10" height="10" viewBox="0 0 12 12" fill="none" aria-hidden>
                        <path
                          d="M2.6 6.3 4.7 8.4 9.4 3.6"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        />
                      </svg>
                    </button>
                  </td>
                  <td
                    className="border-b border-line/60 px-2 h-8 max-w-[260px] truncate text-text/85"
                    title={r.alias}
                  >
                    {r.alias || <span className="text-dim">—</span>}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8 num text-text">
                    {r.host}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8 num text-right text-muted">
                    {r.port}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8" title={geoTitle(r.geo)}>
                    {r.geo?.zh ? (
                      <span className="text-text/90">{r.geo.zh}</span>
                    ) : (
                      <span className="text-dim">—</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8" title={geoTitle(r.geo)}>
                    {r.geo?.en ? (
                      <span className="text-muted">{r.geo.en}</span>
                    ) : (
                      <span className="text-dim">—</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8 num text-[12px] text-dim whitespace-nowrap">
                    {r.source}
                  </td>
                  <td
                    className="border-b border-line/60 px-2 h-8 text-right"
                    title={r.error ?? undefined}
                  >
                    {busy ? (
                      <span className="text-accent text-[12px]">测试中…</span>
                    ) : (
                      latencyCell(r, threshold)
                    )}
                  </td>
                  <td
                    className="border-b border-line/60 px-2 h-8 text-right"
                    title={r.speedError ?? undefined}
                  >
                    {r.speedStatus === "fail" ? (
                      <span className="text-danger text-[12px]">失败</span>
                    ) : r.speed !== null ? (
                      <span className="num text-text/90">
                        {r.speed.toFixed(1)}
                        <span className="text-dim text-[12px] ml-0.5">MB/s</span>
                      </span>
                    ) : (
                      <span className="text-dim">—</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 h-8 text-right">
                    <div className="flex items-center justify-end gap-1 opacity-80 group-hover:opacity-100">
                      <button
                        type="button"
                        onClick={() => void testOne(r.host, r.port, r.alias)}
                        disabled={busy}
                        title={r.error ?? "重测这一条"}
                        className={cn(
                          "h-5 px-1.5 border border-line rounded-[2px] text-[12px] whitespace-nowrap",
                          busy
                            ? "text-dim cursor-wait"
                            : "text-muted hover:text-accent hover:border-accent-dim",
                        )}
                      >
                        {busy ? "测试中" : "重测"}
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
            {padBottom > 0 ? (
              <tr aria-hidden style={{ height: padBottom }}>
                <td colSpan={widths.length} style={{ padding: 0, border: 0 }} />
              </tr>
            ) : null}
          </tbody>
        </table>

        {shown.length === 0 ? (
          <div className="py-16 text-center text-[12px] text-dim">
            没有符合当前筛选的地址
          </div>
        ) : null}
      </div>

      <div className="h-6 shrink-0 flex items-center gap-3 px-4 border-t border-line text-[12px] text-dim num">
        <span>共 {rows.length} 条</span>
        <span>
          成功 {counts.ok} · 超时 {counts.timeout} · 失败 {counts.fail}
        </span>
        <span className="text-line-2">|</span>
        <span>阈值 {threshold}ms</span>
        <span className="ml-auto">
          排序：{sort.key === "latency" ? "延迟" : "速度"}
          {sort.dir === "asc" ? " ↑" : " ↓"}
        </span>
      </div>
    </section>
  );
}
