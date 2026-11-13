import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorMessage, isRecord, isTauriRuntime } from "./utils";

/** 三类日志：操作日志、应用日志、sing-box 日志。 */
export type LogKind = "operation" | "app" | "singbox";

export type LogLevel = "INFO" | "OK" | "WARN" | "ERROR";
export type LevelFilter = "ALL" | LogLevel;

export type LogEntry = {
  kind: LogKind;
  /** ISO 8601 时间戳。 */
  ts: string;
  level: LogLevel;
  /** 操作日志是中文单行；应用日志是原始 JSON 行；sing-box 是原始输出行。 */
  text: string;
  traceId: string | null;
  spanId: string | null;
  event: string | null;
  durationMs: number | null;
};

export type ClearReport = { entries: number; files: number };

/** 界面内存里每类日志最多保留的行数，超出丢弃最旧的。 */
export const MAX_LOG_LINES = 2000;

export const LOG_KINDS: LogKind[] = ["operation", "app", "singbox"];

const LEVELS: LogLevel[] = ["INFO", "OK", "WARN", "ERROR"];

function str(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/** 把后端返回的一行日志归一成界面条目；无法识别时返回 null。 */
export function normalizeLogEntry(raw: unknown): LogEntry | null {
  if (!isRecord(raw)) return null;
  const kind = raw.kind;
  if (kind !== "operation" && kind !== "app" && kind !== "singbox") return null;

  const levelText = typeof raw.level === "string" ? raw.level.toUpperCase() : "";
  const level = LEVELS.find((candidate) => candidate === levelText);
  if (!level) return null;

  const duration = raw.durationMs;
  return {
    kind,
    ts: typeof raw.ts === "string" ? raw.ts : "",
    level,
    text: typeof raw.text === "string" ? raw.text : "",
    traceId: str(raw.traceId),
    spanId: str(raw.spanId),
    event: str(raw.event),
    durationMs:
      typeof duration === "number" && Number.isFinite(duration)
        ? Math.max(0, Math.round(duration))
        : null,
  };
}

function normalizeLines(raw: unknown): LogEntry[] {
  if (!Array.isArray(raw)) return [];
  const lines: LogEntry[] = [];
  for (const item of raw) {
    const entry = normalizeLogEntry(item);
    if (entry) lines.push(entry);
  }
  return lines;
}

/** 把 `get_logs` 的返回拆成三类。 */
export function normalizeLogsSnapshot(raw: unknown): Record<LogKind, LogEntry[]> {
  const source = isRecord(raw) ? raw : {};
  return {
    operation: normalizeLines(source.operation),
    app: normalizeLines(source.app),
    singbox: normalizeLines(source.singbox),
  };
}

/** 只按级别筛选，不改动原数组。 */
export function filterByLevel(lines: LogEntry[], level: LevelFilter): LogEntry[] {
  if (level === "ALL") return lines;
  return lines.filter((line) => line.level === level);
}

/** 追加一行，超过上限时丢弃最旧的。 */
export function appendCapped(
  lines: LogEntry[],
  entry: LogEntry,
  cap: number = MAX_LOG_LINES,
): LogEntry[] {
  const next = [...lines, entry];
  if (next.length <= cap) return next;
  return next.slice(next.length - cap);
}

/** 日志时间只显示时分秒毫秒，与界面原型一致。 */
export function formatLogClock(ts: string): string {
  const at = new Date(ts);
  if (Number.isNaN(at.getTime())) return ts;
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}.${pad(
    at.getMilliseconds(),
    3,
  )}`;
}

/** 持久化端口：默认接 Tauri 命令与事件，测试里换成假实现。 */
export type LogsPersistence = {
  load: () => Promise<unknown>;
  clear: (kind: LogKind) => Promise<unknown>;
  /** 订阅后端推送，返回取消订阅函数。 */
  listen: (onEntry: (entry: LogEntry) => void) => Promise<() => void>;
};

export type LogsStore = {
  lines: Record<LogKind, LogEntry[]>;
  levels: Record<LogKind, LevelFilter>;
  loaded: boolean;
  error: string | null;
  load: () => Promise<void>;
  append: (entry: LogEntry) => void;
  setLevel: (kind: LogKind, level: LevelFilter) => void;
  clear: (kind: LogKind) => Promise<ClearReport | null>;
  subscribe: () => Promise<() => void>;
};

const EMPTY_LINES: Record<LogKind, LogEntry[]> = {
  operation: [],
  app: [],
  singbox: [],
};

const DEFAULT_LEVELS: Record<LogKind, LevelFilter> = {
  operation: "ALL",
  app: "ALL",
  singbox: "ALL",
};

function normalizeReport(raw: unknown): ClearReport {
  if (!isRecord(raw)) return { entries: 0, files: 0 };
  const number = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0;
  return { entries: number(raw.entries), files: number(raw.files) };
}

export function createLogsStore(persistence: LogsPersistence) {
  return create<LogsStore>()((set, get) => ({
    lines: { ...EMPTY_LINES },
    levels: { ...DEFAULT_LEVELS },
    loaded: false,
    error: null,

    load: async () => {
      try {
        const snapshot = normalizeLogsSnapshot(await persistence.load());
        set({ lines: snapshot, loaded: true, error: null });
      } catch (error) {
        // 读失败保留空表，界面照常可用。
        set({ loaded: true, error: errorMessage(error) });
      }
    },

    append: (entry) => {
      const current = get().lines;
      set({
        lines: {
          ...current,
          [entry.kind]: appendCapped(current[entry.kind], entry),
        },
      });
    },

    setLevel: (kind, level) => {
      set({ levels: { ...get().levels, [kind]: level } });
    },

    clear: async (kind) => {
      try {
        const report = normalizeReport(await persistence.clear(kind));
        set({
          lines: { ...get().lines, [kind]: [] },
          error: null,
        });
        return report;
      } catch (error) {
        // 清理失败不清空界面，避免"看起来删了其实没删"。
        set({ error: errorMessage(error) });
        return null;
      }
    },

    subscribe: () =>
      persistence.listen((entry) => {
        get().append(entry);
      }),
  }));
}

/** 生产环境走 Tauri；浏览器里跑原型时保持空表。 */
export const tauriLogsPersistence: LogsPersistence = {
  load: () =>
    isTauriRuntime()
      ? invoke<unknown>("get_logs", { limit: 500 })
      : Promise.resolve(null),
  clear: (kind) =>
    isTauriRuntime()
      ? invoke<unknown>("clear_logs", { kind })
      : Promise.resolve({ entries: 0, files: 0 }),
  listen: (onEntry) => {
    if (!isTauriRuntime()) return Promise.resolve(() => {});
    return listen<unknown>("logs:appended", (event) => {
      const entry = normalizeLogEntry(event.payload);
      if (entry) onEntry(entry);
    });
  },
};

export const useLogsStore = createLogsStore(tauriLogsPersistence);

export const selectLogLines = (state: LogsStore) => state.lines;
export const selectLogLevels = (state: LogsStore) => state.levels;
