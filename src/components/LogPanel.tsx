import { useState } from "react";
import { appLogs, coreLogs, opLogs, type LogLevel, type LogLine } from "../mock";
import { Btn, cn } from "./ui";

type TabId = "op" | "app" | "core";

const tabs: { id: TabId; label: string; lines: LogLine[] }[] = [
  { id: "op", label: "操作日志", lines: opLogs },
  { id: "app", label: "应用日志", lines: appLogs },
  { id: "core", label: "sing-box 日志", lines: coreLogs },
];

const levelStyle: Record<LogLevel, string> = {
  INFO: "text-muted",
  OK: "text-accent",
  WARN: "text-warn",
  ERROR: "text-danger",
};

export default function LogPanel({
  collapsed,
  onToggleCollapse,
}: {
  collapsed: boolean;
  onToggleCollapse: () => void;
}) {
  const [tab, setTab] = useState<TabId>("op");
  const [level, setLevel] = useState<"ALL" | LogLevel>("ALL");
  const [cleared, setCleared] = useState<Record<TabId, number | null>>({
    op: null,
    app: null,
    core: null,
  });
  const [retention, setRetention] = useState({ op: 30, app: 30, core: 30 });

  const current = tabs.find((t) => t.id === tab)!;
  const isCleared = cleared[tab] !== null;
  const lines = isCleared
    ? []
    : current.lines.filter((l) => level === "ALL" || l.level === level);

  return (
    <section className="flex-1 min-h-0 flex flex-col bg-panel border-t border-line">
      <div className="min-h-9 shrink-0 flex flex-wrap items-center gap-x-2 gap-y-1 px-4 py-1 border-b border-line/70">
        <div className="flex items-center gap-0.5">
          {tabs.map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => setTab(t.id)}
              className={cn(
                "h-7 px-2.5 text-[12px] border-b-2 -mb-px transition-colors",
                tab === t.id
                  ? "border-accent text-text"
                  : "border-transparent text-muted hover:text-text",
              )}
            >
              {t.label}
              <span className="num ml-1.5 text-[12px] text-dim">
                {cleared[t.id] !== null ? 0 : t.lines.length}
              </span>
            </button>
          ))}
        </div>

        <div className="ml-auto flex items-center gap-1.5">
          <select
            value={level}
            onChange={(e) => setLevel(e.target.value as typeof level)}
            className="h-7 bg-panel border border-line rounded-[3px] px-1.5 text-[12px] text-muted outline-none focus:border-accent-dim"
          >
            <option value="ALL">全部级别</option>
            <option value="INFO">INFO</option>
            <option value="OK">OK</option>
            <option value="WARN">WARN</option>
            <option value="ERROR">ERROR</option>
          </select>

          <div className="h-7 flex items-center gap-1 border border-line rounded-[3px] px-1.5 bg-panel">
            <span className="text-[12px] text-dim">保留</span>
            <input
              value={retention[tab]}
              onChange={(e) =>
                setRetention((r) => ({
                  ...r,
                  [tab]: Number(e.target.value) || 0,
                }))
              }
              className="num w-7 bg-transparent text-[12px] text-text text-center outline-none"
            />
            <span className="text-[12px] text-dim">天</span>
          </div>

          <Btn
            variant="danger"
            className="h-7 px-2 text-[12px]"
            onClick={() =>
              setCleared((c) => ({ ...c, [tab]: current.lines.length }))
            }
            title={`清空${current.label}的界面内容与磁盘文件`}
          >
            清理日志
          </Btn>
          <Btn variant="ghost" className="h-7 px-2 text-[12px]">
            复制
          </Btn>
          <Btn
            variant="ghost"
            className="h-7 px-2 text-[12px]"
            onClick={onToggleCollapse}
          >
            {collapsed ? "展开" : "折叠"}
          </Btn>
        </div>
      </div>

      {collapsed ? null : (
        <div className="flex-1 min-h-0 overflow-auto overscroll-contain px-4 py-1.5 pb-3 num text-[12px] leading-[1.75]">
          {isCleared ? (
            <div className="flex flex-col gap-0.5">
              <div className="text-dim">
                <span className="text-muted">12:04:02.005</span>
                <span className="text-warn ml-3">[清理]</span>
                <span className="ml-3 text-muted">
                  已清理：{current.label}，删除 {cleared[tab]} 条 / 3 个文件
                </span>
              </div>
              <div className="text-dim">（该类型日志已清空）</div>
            </div>
          ) : (
            lines.map((l, i) => (
              <div
                key={i}
                className="grid grid-cols-[92px_64px_1fr] gap-3 hover:bg-panel-2/60 -mx-1 px-1"
              >
                <span className="text-dim">{l.time}</span>
                <span
                  className={cn(
                    "text-[12px] tracking-wider",
                    levelStyle[l.level],
                  )}
                >
                  {l.level}
                </span>
                <span
                  className={cn(
                    "min-w-0 whitespace-pre-wrap break-words",
                    l.level === "ERROR"
                      ? "text-danger/90"
                      : l.level === "WARN"
                        ? "text-warn/85"
                        : l.level === "OK"
                          ? "text-text/90"
                          : "text-text/75",
                  )}
                >
                  {l.text}
                </span>
              </div>
            ))
          )}
        </div>
      )}
    </section>
  );
}
