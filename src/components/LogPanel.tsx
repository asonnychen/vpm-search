import { useState } from "react";
import {
  filterByLevel,
  formatLogClock,
  selectLogLevels,
  selectLogLines,
  useLogsStore,
  type ClearReport,
  type LevelFilter,
  type LogKind,
  type LogLevel,
} from "../store/logs";
import { selectLogRetention, useSettingsStore } from "../store/settings";
import { Btn, Num, cn } from "./ui";

const tabs: { id: LogKind; label: string }[] = [
  { id: "operation", label: "操作日志" },
  { id: "app", label: "应用日志" },
  { id: "singbox", label: "sing-box 日志" },
];

const levelStyle: Record<LogLevel, string> = {
  INFO: "text-muted",
  OK: "text-accent",
  WARN: "text-warn",
  ERROR: "text-danger",
};

const LEVEL_OPTIONS: LevelFilter[] = ["ALL", "INFO", "OK", "WARN", "ERROR"];

/** 三类日志各自的保留天数字段。 */
const retentionField = {
  operation: "operationDays",
  app: "appDays",
  singbox: "singboxDays",
} as const;

export default function LogPanel({
  collapsed,
  onToggleCollapse,
}: {
  collapsed: boolean;
  onToggleCollapse: () => void;
}) {
  const lines = useLogsStore(selectLogLines);
  const levels = useLogsStore(selectLogLevels);
  const setLevel = useLogsStore((state) => state.setLevel);
  const clear = useLogsStore((state) => state.clear);
  const retention = useSettingsStore(selectLogRetention);
  const patch = useSettingsStore((state) => state.patch);

  const [tab, setTab] = useState<LogKind>("operation");
  const [lastClear, setLastClear] = useState<{ kind: LogKind; report: ClearReport } | null>(
    null,
  );
  const [copied, setCopied] = useState(false);

  const current = tabs.find((t) => t.id === tab)!;
  const visible = filterByLevel(lines[tab], levels[tab]);

  const onClear = async () => {
    const report = await clear(tab);
    if (report) setLastClear({ kind: tab, report });
  };

  const onCopy = async () => {
    const text = visible
      .map((line) => `${formatLogClock(line.ts)} ${line.level} ${line.text}`)
      .join("\n");
    try {
      await navigator.clipboard?.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // 剪贴板不可用时保持静默，不打断操作。
    }
  };

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
              <span className="num ml-1.5 text-[12px] text-dim">{lines[t.id].length}</span>
            </button>
          ))}
        </div>

        <div className="ml-auto flex items-center gap-1.5">
          {lastClear && lastClear.kind === tab ? (
            <span className="text-[12px] text-dim num">
              已清理 {lastClear.report.entries} 条 / {lastClear.report.files} 个文件
            </span>
          ) : null}

          <select
            value={levels[tab]}
            onChange={(e) => setLevel(tab, e.target.value as LevelFilter)}
            className="h-7 bg-panel border border-line rounded-[3px] px-1.5 text-[12px] text-muted outline-none focus:border-accent-dim"
          >
            {LEVEL_OPTIONS.map((level) => (
              <option key={level} value={level}>
                {level === "ALL" ? "全部级别" : level}
              </option>
            ))}
          </select>

          <div className="h-7 flex items-center gap-1">
            <span className="text-[12px] text-dim">保留</span>
            <Num
              value={retention[retentionField[tab]]}
              unit="天"
              width="76px"
              min={1}
              max={3650}
              onChange={(value) => {
                patch({ logRetention: { [retentionField[tab]]: value } });
                // 保留天数要立即生效，不等防抖。
                void useSettingsStore.getState().flush();
              }}
            />
          </div>

          <Btn
            variant="danger"
            className="h-7 px-2 text-[12px]"
            onClick={() => void onClear()}
            title={`清空${current.label}的界面内容与磁盘文件`}
          >
            清理日志
          </Btn>
          <Btn
            variant="ghost"
            className="h-7 px-2 text-[12px]"
            onClick={() => void onCopy()}
            title={`复制当前${current.label}的可见内容`}
          >
            {copied ? "已复制" : "复制"}
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
          {visible.length === 0 ? (
            <div className="text-dim">
              {lines[tab].length === 0
                ? `（${current.label}为空）`
                : `（没有 ${levels[tab]} 级别的${current.label}）`}
            </div>
          ) : (
            visible.map((line, index) => (
              <div
                key={`${line.ts}-${index}`}
                className="grid grid-cols-[92px_64px_1fr] gap-3 hover:bg-panel-2/60 -mx-1 px-1"
              >
                <span className="text-dim">{formatLogClock(line.ts)}</span>
                <span className={cn("text-[12px] tracking-wider", levelStyle[line.level])}>
                  {line.level}
                </span>
                <span
                  className={cn(
                    "min-w-0 whitespace-pre-wrap break-words",
                    line.level === "ERROR"
                      ? "text-danger/90"
                      : line.level === "WARN"
                        ? "text-warn/85"
                        : line.level === "OK"
                          ? "text-text/90"
                          : "text-text/75",
                  )}
                >
                  {line.text}
                </span>
              </div>
            ))
          )}
        </div>
      )}
    </section>
  );
}
