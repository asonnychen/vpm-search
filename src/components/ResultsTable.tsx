import { useEffect, useMemo, useRef, useState } from "react";
import { MAX_LATENCY, rows, type Row } from "../mock";
import { cn } from "./ui";

const filters = [
  { id: "all", label: "全部", count: 128 },
  { id: "ok", label: "成功", count: 8 },
  { id: "timeout", label: "超时", count: 1 },
  { id: "fail", label: "失败", count: 1 },
  { id: "slow", label: "超阈值", count: 1 },
] as const;

type FilterId = (typeof filters)[number]["id"];

function latencyCell(row: Row) {
  if (row.status === "timeout") {
    return <span className="text-warn text-[12px]">超时</span>;
  }
  if (row.status === "fail") {
    return <span className="text-danger text-[12px]">失败</span>;
  }
  const over = row.latency! > MAX_LATENCY;
  return (
    <span className="inline-flex items-center justify-end gap-1.5">
      <span className="w-10 h-1 bg-panel-2 ring-1 ring-line rounded-full overflow-hidden">
        <span
          className={cn("block h-full rounded-full", over ? "bg-danger" : "bg-accent")}
          style={{ width: `${Math.min(100, (row.latency! / MAX_LATENCY) * 100)}%` }}
        />
      </span>
      <span className={cn("num", over ? "text-danger" : "text-text")}>
        {row.latency}
        <span className="text-dim text-[12px] ml-0.5">ms</span>
      </span>
    </span>
  );
}

export default function ResultsTable() {
  const [filter, setFilter] = useState<FilterId>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string[]>(["1"]);
  const [sort, setSort] = useState<{ key: "latency" | "speed"; dir: "asc" | "desc" }>({
    key: "latency",
    dir: "asc",
  });

  const toggleSort = (key: "latency" | "speed") =>
    setSort((s) =>
      s.key === key ? { key, dir: s.dir === "asc" ? "desc" : "asc" } : { key, dir: "asc" },
    );

  // 列宽：拖拽表头右缘调整，索引与列一一对应
  const COL_WIDTHS = [64, 60, 172, 140, 58, 54, 84, 96, 54, 46, 156, 112, 88, 84];
  const [widths, setWidths] = useState<number[]>(COL_WIDTHS);
  const resizeRef = useRef<{ i: number; x: number; w: number } | null>(null);

  useEffect(() => {
    const move = (e: MouseEvent) => {
      const r = resizeRef.current;
      if (!r) return;
      const next = Math.max(48, r.w + (e.clientX - r.x));
      setWidths((ws) => ws.map((w, i) => (i === r.i ? next : w)));
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
  }, []);

  const startResize = (i: number) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    resizeRef.current = { i, x: e.clientX, w: widths[i] };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  };

  const shown = useMemo(() => {
    const list = rows.filter((r) => {
      if (filter === "ok" && r.status !== "ok") return false;
      if (filter === "timeout" && r.status !== "timeout") return false;
      if (filter === "fail" && r.status !== "fail") return false;
      if (filter === "slow" && !(r.latency && r.latency > MAX_LATENCY)) return false;
      if (query) {
        const q = query.toLowerCase();
        return (
          r.host.toLowerCase().includes(q) ||
          r.alias.toLowerCase().includes(q) ||
          r.geoZh.includes(q) ||
          r.geoEn.toLowerCase().includes(q)
        );
      }
      return true;
    });

    // 空值（超时 / 失败）始终排在末尾，不参与升降序翻转
    return list.sort((a, b) => {
      const av = a[sort.key];
      const bv = b[sort.key];
      if (av === null && bv === null) return 0;
      if (av === null) return 1;
      if (bv === null) return -1;
      return sort.dir === "asc" ? av - bv : bv - av;
    });
  }, [filter, query, sort]);

  const toggle = (id: string) =>
    setSelected((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]));

  const shownIds = shown.map((r) => r.id);
  const allShown = shownIds.length > 0 && shownIds.every((id) => selected.includes(id));
  const someShown = shownIds.some((id) => selected.includes(id));

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

  return (
    <section className="flex-1 min-h-0 flex flex-col bg-panel">
      <div className="min-h-10 shrink-0 flex flex-wrap items-center gap-x-2 gap-y-1.5 px-4 py-1.5 border-b border-line bg-panel">
        <div className="flex items-center gap-0.5 border border-line rounded-[3px] p-0.5 bg-canvas/60">
          {filters.map((f) => (
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
                {f.count}
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
          <span>
            最快 <span className="text-accent">38</span>ms
          </span>
          <span>
            中位 <span className="text-text">88</span>ms
          </span>
          <span>
            平均 <span className="text-text">6.9</span>MB/s
          </span>
          <span className="text-line-2">|</span>
          <span>
            已选 <span className="text-text">{selected.length}</span>
          </span>
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-auto border border-line rounded-[3px] bg-panel">
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
                { label: "类型" },
                { label: "别名" },
                { label: "地址" },
                { label: "端口", align: "right" },
                { label: "传输" },
                { label: "归属地(中)" },
                { label: "归属地(英)" },
                { label: "机房", align: "right" },
                { label: "TLS", align: "right" },
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
            {shown.map((r) => {
              const isSel = selected.includes(r.id);
              const slow = r.latency !== null && r.latency > MAX_LATENCY;
              return (
                <tr
                  key={r.id}
                  className={cn(
                    "group transition-colors",
                    isSel ? "bg-accent/[0.07]" : "hover:bg-panel-2/70",
                  )}
                >
                  <td className="border-b border-line/60 px-2 py-1.5 w-8">
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
                  <td className="border-b border-line/60 px-2 py-1.5">
                    <span className="inline-flex h-5 items-center px-1.5 rounded-[2px] border border-line-2/70 text-[12px] num text-muted">
                      {r.type}
                    </span>
                  </td>
                  <td
                    className="border-b border-line/60 px-2 py-1.5 max-w-[260px] truncate text-text/85"
                    title={r.alias}
                  >
                    {r.alias}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 num text-text">
                    {r.host}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 num text-right text-muted">
                    {r.port}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 num text-muted">
                    {r.transport}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5">
                    {r.geoZh ? (
                      <span className="text-text/90">{r.geoZh}</span>
                    ) : (
                      <span className="text-dim">—</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 text-muted">
                    {r.geoEn || <span className="text-dim">—</span>}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 num text-right">
                    {r.colo ? (
                      <span className="text-[12px] px-1 py-0.5 border border-line-2/60 rounded-[2px] text-muted">
                        {r.colo}
                      </span>
                    ) : (
                      <span className="text-dim">—</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 text-right">
                    {r.tls ? (
                      <span className="text-accent/80 text-[12px]">开</span>
                    ) : (
                      <span className="text-dim text-[12px]">关</span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 num text-[12px] text-dim whitespace-nowrap">
                    {r.source}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 text-right">
                    {latencyCell(r)}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 text-right">
                    {r.speed === null ? (
                      <span className="text-dim">—</span>
                    ) : (
                      <span className="num text-text/90">
                        {r.speed.toFixed(1)}
                        <span className="text-dim text-[12px] ml-0.5">MB/s</span>
                      </span>
                    )}
                  </td>
                  <td className="border-b border-line/60 px-2 py-1.5 text-right">
                    <div className="flex items-center justify-end gap-1 opacity-80 group-hover:opacity-100">
                      <button
                        type="button"
                        className="h-5 px-1.5 border border-line rounded-[2px] text-[12px] text-muted hover:text-accent hover:border-accent-dim whitespace-nowrap"
                      >
                        重测
                      </button>
                      <button
                        type="button"
                        className="size-5 grid place-items-center border border-line rounded-[2px] text-dim hover:text-text"
                        title="详情"
                      >
                        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                          <circle cx="12" cy="12" r="9" />
                          <path d="M12 11v5M12 8h.01" />
                        </svg>
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>

        {shown.length === 0 ? (
          <div className="py-16 text-center text-[12px] text-dim">
            没有符合当前筛选的地址
          </div>
        ) : null}
      </div>

      <div className="h-6 shrink-0 flex items-center gap-3 px-4 border-t border-line text-[12px] text-dim num">
        <span>共 128 条</span>
        <span>成功 8 · 超时 1 · 失败 1</span>
        <span className="text-line-2">|</span>
        <span>阈值 {MAX_LATENCY}ms</span>
        <span className="ml-auto">
          排序：{sort.key === "latency" ? "延迟" : "速度"}
          {sort.dir === "asc" ? " ↑" : " ↓"}
        </span>
      </div>
    </section>
  );
}
