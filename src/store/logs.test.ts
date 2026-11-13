import { describe, expect, it, vi } from "vitest";
import {
  MAX_LOG_LINES,
  appendCapped,
  createLogsStore,
  filterByLevel,
  formatLogClock,
  normalizeLogEntry,
  normalizeLogsSnapshot,
  type LogEntry,
  type LogsPersistence,
} from "./logs";

function entry(overrides: Partial<LogEntry> = {}): LogEntry {
  return {
    kind: "operation",
    ts: "2026-11-13T12:03:41.118Z",
    level: "INFO",
    text: "抓取完成",
    traceId: null,
    spanId: null,
    event: null,
    durationMs: null,
    ...overrides,
  };
}

function fakePersistence(overrides: Partial<LogsPersistence> = {}): LogsPersistence {
  return {
    load: vi.fn(async () => null),
    clear: vi.fn(async () => ({ entries: 0, files: 0 })),
    listen: vi.fn(async () => () => {}),
    ...overrides,
  };
}

describe("日志过滤", () => {
  it("按级别过滤，ALL 保留全部", () => {
    const lines = [
      entry({ level: "INFO" }),
      entry({ level: "WARN" }),
      entry({ level: "ERROR" }),
      entry({ level: "OK" }),
    ];
    expect(filterByLevel(lines, "ALL")).toHaveLength(4);
    expect(filterByLevel(lines, "WARN").map((l) => l.level)).toEqual(["WARN"]);
    expect(filterByLevel(lines, "ERROR")).toHaveLength(1);
    expect(filterByLevel(lines, "OK")).toHaveLength(1);
  });

  it("空列表与未知级别不抛异常", () => {
    expect(filterByLevel([], "ALL")).toEqual([]);
    expect(filterByLevel([entry()], "INFO")).toHaveLength(1);
  });
});

describe("环形缓冲", () => {
  it("超过上限时丢弃最旧的", () => {
    const lines = Array.from({ length: MAX_LOG_LINES }, (_, i) =>
      entry({ text: `第 ${i} 条` }),
    );
    const next = appendCapped(lines, entry({ text: "最新一条" }));
    expect(next).toHaveLength(MAX_LOG_LINES);
    expect(next[0]!.text).toBe("第 1 条");
    expect(next[next.length - 1]!.text).toBe("最新一条");
  });

  it("未达上限时直接追加", () => {
    const next = appendCapped([entry({ text: "a" })], entry({ text: "b" }), 10);
    expect(next.map((l) => l.text)).toEqual(["a", "b"]);
  });
});

describe("日志归一化", () => {
  it("坏行被丢弃，合法行补齐空字段", () => {
    expect(normalizeLogEntry(null)).toBeNull();
    expect(normalizeLogEntry({ kind: "乱写", level: "INFO" })).toBeNull();
    expect(normalizeLogEntry({ kind: "app", level: "噪音" })).toBeNull();

    const parsed = normalizeLogEntry({
      kind: "app",
      level: "warn",
      ts: "2026-11-13T00:00:00.000Z",
      text: "{}",
      traceId: "tr-1",
      durationMs: 12.4,
    });
    expect(parsed).toEqual({
      kind: "app",
      level: "WARN",
      ts: "2026-11-13T00:00:00.000Z",
      text: "{}",
      traceId: "tr-1",
      spanId: null,
      event: null,
      durationMs: 12,
    });
  });

  it("快照按三类拆开，缺字段当空表", () => {
    const snapshot = normalizeLogsSnapshot({
      operation: [{ kind: "operation", level: "INFO", ts: "t", text: "一" }],
      singbox: "不是数组",
    });
    expect(snapshot.operation).toHaveLength(1);
    expect(snapshot.app).toEqual([]);
    expect(snapshot.singbox).toEqual([]);
  });

  it("时间只显示时分秒毫秒", () => {
    expect(formatLogClock("2026-11-13T12:03:41.118Z")).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
    expect(formatLogClock("乱码")).toBe("乱码");
  });
});

describe("日志 store", () => {
  it("启动时读回三类日志并保持顺序", async () => {
    const persistence = fakePersistence({
      load: async () => ({
        operation: [{ kind: "operation", level: "OK", ts: "t1", text: "第一条" }],
        app: [{ kind: "app", level: "INFO", ts: "t2", text: "{}" }],
      }),
    });
    const store = createLogsStore(persistence);

    await store.getState().load();

    const state = store.getState();
    expect(state.loaded).toBe(true);
    expect(state.lines.operation[0]!.text).toBe("第一条");
    expect(state.lines.app).toHaveLength(1);
    expect(state.lines.singbox).toEqual([]);
  });

  it("推送的日志按类型追加并遵守上限", () => {
    const store = createLogsStore(fakePersistence());
    for (let i = 0; i < MAX_LOG_LINES + 5; i += 1) {
      store.getState().append(entry({ kind: "app", text: `第 ${i} 条` }));
    }
    const lines = store.getState().lines.app;
    expect(lines).toHaveLength(MAX_LOG_LINES);
    expect(lines[0]!.text).toBe("第 5 条");
    expect(store.getState().lines.operation).toEqual([]);
  });

  it("清理只清空该类日志并记录结果", async () => {
    const clear = vi.fn(async () => ({ entries: 3, files: 1 }));
    const store = createLogsStore(fakePersistence({ clear }));
    store.getState().append(entry({ kind: "operation" }));
    store.getState().append(entry({ kind: "singbox", text: "原始行" }));

    const report = await store.getState().clear("operation");

    expect(clear).toHaveBeenCalledWith("operation");
    expect(report).toEqual({ entries: 3, files: 1 });
    expect(store.getState().lines.operation).toEqual([]);
    expect(store.getState().lines.singbox).toHaveLength(1);
  });

  it("清理失败保留界面内容并记录错误", async () => {
    const store = createLogsStore(
      fakePersistence({
        clear: async () => {
          throw new Error("磁盘被占用");
        },
      }),
    );
    store.getState().append(entry({ kind: "operation" }));

    await store.getState().clear("operation");

    expect(store.getState().lines.operation).toHaveLength(1);
    expect(store.getState().error).toContain("磁盘被占用");
  });

  it("按类型记住级别过滤", () => {
    const store = createLogsStore(fakePersistence());
    store.getState().setLevel("app", "ERROR");
    expect(store.getState().levels.app).toBe("ERROR");
    expect(store.getState().levels.operation).toBe("ALL");
  });
});
