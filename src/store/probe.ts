import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorMessage, isRecord, isTauriRuntime } from "./utils";

/** 单条测试的结论，与后端 `ProbeStatus` 一致。 */
export type ProbeStatus = "ok" | "timeout" | "fail" | "untested";

/** 一条地址的测试结果。 */
export type ProbeResult = {
  status: ProbeStatus;
  /** 成功时的往返毫秒数。 */
  latencyMs: number | null;
  /** 失败或超时的原因。 */
  error: string | null;
};

/** 表格行的键，与 `ResultsTable` 一致。 */
export function probeKey(host: string, port: number): string {
  return `${host}:${port}`;
}

/** 把后端返回的任意 JSON 归一成结果；坏数据一律当失败。 */
export function normalizeProbeResult(raw: unknown): ProbeResult {
  if (!isRecord(raw)) {
    return { status: "fail", latencyMs: null, error: "测试返回了无法识别的结果" };
  }
  const status = raw.status;
  const error =
    typeof raw.error === "string" && raw.error !== "" ? raw.error : null;

  if (status === "timeout") {
    return { status: "timeout", latencyMs: null, error };
  }
  if (status === "untested") {
    // 停止后没轮到的行，不伪装成失败。
    return { status: "untested", latencyMs: null, error: null };
  }
  if (status === "ok") {
    const latency = raw.latencyMs;
    if (typeof latency !== "number" || !Number.isFinite(latency) || latency < 0) {
      // 成功却没有延迟是坏数据，按失败处理，不显示 0ms。
      return { status: "fail", latencyMs: null, error: error ?? "结果缺少延迟" };
    }
    return { status: "ok", latencyMs: Math.max(1, Math.round(latency)), error };
  }
  return { status: "fail", latencyMs: null, error };
}

/** 批量测试里一条地址的结果。 */
export type BatchRow = ProbeResult & {
  host: string;
  port: number;
  alias: string;
};

/** 一轮批量测试的统计。 */
export type BatchCounts = {
  ok: number;
  timeout: number;
  fail: number;
  untested: number;
};

/** 一轮批量测试的完整结果。 */
export type BatchReport = {
  runId: string;
  total: number;
  completed: number;
  stopped: boolean;
  counts: BatchCounts;
  rows: BatchRow[];
};

/** 后端通过事件推送的进度。 */
export type BatchProgress = {
  runId: string;
  /** 这一轮是延迟测试、测速还是归属地查询。 */
  mode?: "latency" | "speed" | "geo";
  completed: number;
  total: number;
};

/** 单条实时结果事件的类型：延迟、测速、归属地三条链路各推各的。 */
export type ProbeRowKind = "delay" | "speed" | "geo";

/**
 * 后端逐条推送的实时结果（`probe:row`）。
 * 延迟、测速、归属地以及单条重测都走这个事件。
 */
export type ProbeRowEvent = {
  runId: string;
  kind: ProbeRowKind;
  host: string;
  port: number;
  status: ProbeStatus;
  latencyMs: number | null;
  speedMbps: number | null;
  countryZh: string | null;
  countryEn: string | null;
  /** 历史字段：机房已废弃，后端恒为 null。 */
  colo: string | null;
  error: string | null;
};

/** 把后端推来的任意 JSON 归一成一条实时结果；缺 host/port 的直接丢弃。 */
export function normalizeProbeRow(raw: unknown): ProbeRowEvent | null {
  if (!isRecord(raw)) return null;

  const host = str(raw.host).trim();
  const port = Math.round(num(raw.port));
  if (host === "" || port <= 0 || port > 65535) return null;

  const kind: ProbeRowKind =
    raw.kind === "speed" ? "speed" : raw.kind === "geo" ? "geo" : "delay";
  const status: ProbeStatus =
    raw.status === "ok" ||
    raw.status === "timeout" ||
    raw.status === "fail" ||
    raw.status === "untested"
      ? raw.status
      : "fail";

  const latency = raw.latencyMs;
  const speed = raw.speedMbps;
  const text = (value: unknown): string | null =>
    typeof value === "string" && value.trim() !== "" ? value : null;

  return {
    runId: str(raw.runId),
    kind,
    host,
    port,
    status,
    latencyMs:
      typeof latency === "number" && Number.isFinite(latency) && latency >= 0
        ? Math.max(1, Math.round(latency))
        : null,
    speedMbps:
      typeof speed === "number" && Number.isFinite(speed) && speed > 0 ? speed : null,
    countryZh: text(raw.countryZh),
    countryEn: text(raw.countryEn),
    colo: text(raw.colo),
    error: text(raw.error),
  };
}

/** 把一条实时结果并进本地结果表；返回是否有字段被改动。 */
function mergeProbeRow(
  row: ProbeRowEvent,
  results: Record<string, ProbeResult>,
  speeds: Record<string, SpeedResult>,
  geos: Record<string, GeoResult>,
): boolean {
  const key = probeKey(row.host, row.port);

  if (row.kind === "delay") {
    const next = normalizeProbeResult({
      status: row.status,
      latencyMs: row.latencyMs,
      error: row.error,
    });
    results[key] = next;
    return true;
  }

  if (row.kind === "speed") {
    // 未轮到的行不覆盖已有速度，与整轮结果的语义保持一致。
    if (row.status === "untested") return false;
    const ok = row.status === "ok" && row.speedMbps !== null;
    speeds[key] = {
      status: ok ? "ok" : "fail",
      mbps: ok ? row.speedMbps : null,
      error: ok ? row.error : (row.error ?? "测速结果缺少速度"),
    };
    return true;
  }

  // 归属地：只有真正拿到地名才覆盖，失败不抹掉上一次的结果。
  const zh = row.countryZh ?? "";
  const en = row.countryEn ?? "";
  if (row.status !== "ok" || (zh === "" && en === "")) return false;
  geos[key] = {
    ip: null,
    country: null,
    zh,
    en,
    asn: null,
    // 实时事件不带来源与 IP，留空等整轮结果补齐。
    source: "",
  };
  return true;
}

/**
 * 实时结果的合并窗口（毫秒）。
 *
 * 整池上千行时 `probe:row` 会非常密集，150–200ms 一批既能看清进度，
 * 又不会每条事件都触发整表重排；收尾的整轮报告不受这里影响，立即覆盖。
 */
export const PROBE_ROW_FLUSH_MS = 180;

/**
 * 高频实时结果的合并队列：同一条地址的同一类结果只保留最后一次，
 * 每 `intervalMs` 最多触发一次写入，避免每条事件都整表重排。
 */
export type ProbeRowQueue = {
  push: (row: ProbeRowEvent) => void;
  /** 立即写出待处理的结果，用于测试与收尾。 */
  flush: () => void;
  /** 丢弃待处理的结果并取消定时器。 */
  clear: () => void;
};

export function createProbeRowQueue(
  handler: (rows: ProbeRowEvent[]) => void,
  intervalMs = PROBE_ROW_FLUSH_MS,
): ProbeRowQueue {
  let pending = new Map<string, ProbeRowEvent>();
  let timer: ReturnType<typeof setTimeout> | null = null;

  const flush = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    if (pending.size === 0) return;
    const rows = [...pending.values()];
    pending = new Map();
    handler(rows);
  };

  return {
    push: (row) => {
      // 同一地址的同一类结果合并成最后一次，减少渲染次数。
      pending.set(`${row.kind}:${probeKey(row.host, row.port)}`, row);
      if (timer === null) timer = setTimeout(flush, intervalMs);
    },
    flush,
    clear: () => {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      pending = new Map();
    },
  };
}

export type BatchState = {
  /** 正在跑一轮。 */
  running: boolean;
  /** 已经点过停止，等待在跑的采样收尾。 */
  stopping: boolean;
  /** 这一轮的类型，决定按钮文案与进度说明。 */
  mode: "latency" | "speed" | "geo";
  runId: string | null;
  completed: number;
  total: number;
  stopped: boolean;
};

/** 测速状态，与延迟状态互不影响。 */
export type SpeedStatus = "ok" | "fail" | "untested";

/** 一条地址的测速结果。 */
export type SpeedResult = {
  status: SpeedStatus;
  /** MB/s，保留一位小数；未测或失败时为 null。 */
  mbps: number | null;
  error: string | null;
};

/** 一条测速目标，界面传选中行时使用。 */
export type SpeedTarget = { host: string; port: number };

/** 批量测速里一条地址的结果。 */
export type SpeedRow = SpeedResult & {
  host: string;
  port: number;
  alias: string;
};

/** 一轮批量测速的统计。 */
export type SpeedCounts = {
  ok: number;
  fail: number;
  untested: number;
};

/** 一轮批量测速的完整结果。 */
export type SpeedReport = {
  runId: string;
  total: number;
  completed: number;
  stopped: boolean;
  counts: SpeedCounts;
  rows: SpeedRow[];
};

/** 一条地址的归属地结果，与延迟、测速分开保存。 */
export type GeoResult = {
  /** 这条地址解析出的 IP，也就是被查询的对象。 */
  ip: string | null;
  /** ISO 国家码。 */
  country: string | null;
  /** 归属地中文名。 */
  zh: string;
  /** 归属地英文名。 */
  en: string;
  asn: number | null;
  /** 数据来源：offline / online / cache。 */
  source: string;
};

/** 归属地查询里一条地址的结果。 */
export type GeoRow = GeoResult & {
  host: string;
  port: number;
  alias: string;
  status: "ok" | "fail" | "untested";
  error: string | null;
};

/** 一轮归属地查询的统计。 */
export type GeoCounts = {
  ok: number;
  fail: number;
  untested: number;
};

/** 一轮归属地查询的完整结果。 */
export type GeoReport = {
  runId: string;
  total: number;
  completed: number;
  stopped: boolean;
  counts: GeoCounts;
  rows: GeoRow[];
};

export const EMPTY_GEO_REPORT: GeoReport = {
  runId: "",
  total: 0,
  completed: 0,
  stopped: false,
  counts: { ok: 0, fail: 0, untested: 0 },
  rows: [],
};

export const EMPTY_BATCH_REPORT: BatchReport = {  runId: "",
  total: 0,
  completed: 0,
  stopped: false,
  counts: { ok: 0, timeout: 0, fail: 0, untested: 0 },
  rows: [],
};

export const EMPTY_BATCH_STATE: BatchState = {
  running: false,
  stopping: false,
  mode: "latency",
  runId: null,
  completed: 0,
  total: 0,
  stopped: false,
};

export const EMPTY_SPEED_REPORT: SpeedReport = {
  runId: "",
  total: 0,
  completed: 0,
  stopped: false,
  counts: { ok: 0, fail: 0, untested: 0 },
  rows: [],
};

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function num(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function count(value: unknown): number {
  return Math.max(0, Math.round(num(value)));
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/** 把后端返回的任意 JSON 归一成一轮结果；坏字段只丢自己。 */
export function normalizeBatchReport(raw: unknown): BatchReport {
  if (!isRecord(raw)) return EMPTY_BATCH_REPORT;

  const rows: BatchRow[] = Array.isArray(raw.rows)
    ? raw.rows.filter(isRecord).map((item) => ({
        host: str(item.host),
        port: Math.round(num(item.port)),
        alias: str(item.alias),
        ...normalizeProbeResult(item),
      }))
    : [];

  const counts = isRecord(raw.counts) ? raw.counts : {};
  return {
    runId: str(raw.runId),
    total: count(raw.total),
    completed: count(raw.completed),
    stopped: bool(raw.stopped, false),
    counts: {
      ok: count(counts.ok),
      timeout: count(counts.timeout),
      fail: count(counts.fail),
      untested: count(counts.untested),
    },
    rows: rows.filter((row) => row.host !== "" && row.port > 0),
  };
}

/** 把后端返回的任意 JSON 归一成一轮测速结果；坏字段只丢自己。 */
export function normalizeSpeedReport(raw: unknown): SpeedReport {
  if (!isRecord(raw)) return EMPTY_SPEED_REPORT;

  const rows: SpeedRow[] = Array.isArray(raw.rows)
    ? raw.rows.filter(isRecord).map((item) => {
        const status = str(item.status, "fail");
        const error =
          typeof item.error === "string" && item.error !== "" ? item.error : null;
        const mbps = num(item.mbps, 0);

        if (status === "untested") {
          return {
            host: str(item.host),
            port: Math.round(num(item.port)),
            alias: str(item.alias),
            status: "untested" as const,
            mbps: null,
            error: null,
          };
        }
        // 成功却没有正速度是坏数据，按失败处理，绝不显示 0 MB/s。
        if (status === "ok" && mbps > 0) {
          return {
            host: str(item.host),
            port: Math.round(num(item.port)),
            alias: str(item.alias),
            status: "ok" as const,
            mbps,
            error,
          };
        }
        return {
          host: str(item.host),
          port: Math.round(num(item.port)),
          alias: str(item.alias),
          status: "fail" as const,
          mbps: null,
          error: error ?? "测速结果缺少速度",
        };
      })
    : [];

  const counts = isRecord(raw.counts) ? raw.counts : {};
  return {
    runId: str(raw.runId),
    total: count(raw.total),
    completed: count(raw.completed),
    stopped: bool(raw.stopped, false),
    counts: {
      ok: count(counts.ok),
      fail: count(counts.fail),
      untested: count(counts.untested),
    },
    rows: rows.filter((row) => row.host !== "" && row.port > 0),
  };
}

/** 把后端返回的任意 JSON 归一成一轮归属地结果；坏字段只丢自己。 */
export function normalizeGeoReport(raw: unknown): GeoReport {
  if (!isRecord(raw)) return EMPTY_GEO_REPORT;

  const rows: GeoRow[] = Array.isArray(raw.rows)
    ? raw.rows.filter(isRecord).map((item) => {
        const host = str(item.host);
        const port = Math.round(num(item.port));
        const alias = str(item.alias);
        const error =
          typeof item.error === "string" && item.error !== "" ? item.error : null;
        const zh = str(item.zh);
        const en = str(item.en);

        // 状态与归属地以「有没有中英文名」为准：后端说成功但没名字是坏数据。
        if (item.status === "untested") {
          return { host, port, alias, status: "untested" as const, error: null,
            ip: null, country: null, zh: "", en: "", asn: null, source: "" };
        }
        if (item.status !== "ok" || (zh === "" && en === "")) {
          return { host, port, alias, status: "fail" as const,
            error: error ?? "归属地查询失败",
            ip: null, country: null, zh: "", en: "", asn: null, source: "" };
        }

        const asn = num(item.asn, 0);
        return {
          host,
          port,
          alias,
          status: "ok" as const,
          error: null,
          ip: str(item.ip) || null,
          country: str(item.country) || null,
          zh,
          en,
          asn: asn > 0 ? Math.round(asn) : null,
          source: str(item.source),
        };
      })
    : [];

  const counts = isRecord(raw.counts) ? raw.counts : {};
  return {
    runId: str(raw.runId),
    total: count(raw.total),
    completed: count(raw.completed),
    stopped: bool(raw.stopped, false),
    counts: {
      ok: count(counts.ok),
      fail: count(counts.fail),
      untested: count(counts.untested),
    },
    rows: rows.filter((row) => row.host !== "" && row.port > 0),
  };
}

/** 把 `host:port` 形式的选中行拆成测速目标。 */
export function speedTargetsFromIds(ids: string[]): SpeedTarget[] {
  return ids.flatMap((id) => {
    const index = id.lastIndexOf(":");
    if (index <= 0) return [];
    const port = Number(id.slice(index + 1));
    if (!Number.isInteger(port) || port <= 0 || port > 65535) return [];
    return [{ host: id.slice(0, index), port }];
  });
}

/** 持久化端口：默认接 Tauri 命令，测试里换成假实现。 */
export type ProbePersistence = {
  probe: (host: string, port: number, alias: string) => Promise<unknown>;
  startBatch: () => Promise<unknown>;
  speedBatch: (targets?: SpeedTarget[]) => Promise<unknown>;
  geoBatch: (targets?: SpeedTarget[]) => Promise<unknown>;
  cancelBatch: () => Promise<unknown>;
};

export type ProbeStore = {
  /** 按 `host:port` 索引的结果，未测的行不在表里。 */
  results: Record<string, ProbeResult>;
  /** 按 `host:port` 索引的测速结果，与延迟分开保存。 */
  speeds: Record<string, SpeedResult>;
  /** 按 `host:port` 索引的归属地结果，与延迟、测速互不影响。 */
  geos: Record<string, GeoResult>;
  /** 正在测试的行。 */
  testing: Record<string, boolean>;
  /** 批量测试的进度与状态。 */
  batch: BatchState;
  /** 最近一次失败原因，用于提示 sidecar 日志路径。 */
  error: string | null;
  testOne: (host: string, port: number, alias?: string) => Promise<ProbeResult>;
  startBatch: () => Promise<void>;
  startSpeedBatch: (targets?: SpeedTarget[]) => Promise<void>;
  startGeoBatch: (targets?: SpeedTarget[]) => Promise<void>;
  cancelBatch: () => Promise<void>;
  applyProgress: (progress: BatchProgress) => void;
  /** 把一批实时结果写进对应行（已按 `host:port` 合并）。 */
  applyRows: (rows: ProbeRowEvent[]) => void;
};

export function createProbeStore(persistence: ProbePersistence) {
  // 已经收尾的轮次：迟到的事件不再写入，避免行停在中间态。
  const finishedRuns: string[] = [];
  const markFinished = (runId: string) => {
    if (runId === "" || finishedRuns.includes(runId)) return;
    finishedRuns.push(runId);
    if (finishedRuns.length > 8) finishedRuns.shift();
  };

  return create<ProbeStore>()((set, get) => ({
    results: {},
    speeds: {},
    geos: {},
    testing: {},
    batch: { ...EMPTY_BATCH_STATE },
    error: null,

    testOne: async (host, port, alias = "") => {
      const key = probeKey(host, port);
      set({
        testing: { ...get().testing, [key]: true },
        error: null,
      });

      let result: ProbeResult;
      try {
        result = normalizeProbeResult(await persistence.probe(host, port, alias));
      } catch (error) {
        result = { status: "fail", latencyMs: null, error: errorMessage(error) };
      }

      const testing = { ...get().testing };
      delete testing[key];
      set({
        results: { ...get().results, [key]: result },
        testing,
        // 只有失败才留提示；成功的测试不该覆盖上一次的错误提示以外的内容。
        error: result.status === "fail" ? result.error : null,
      });
      return result;
    },

    startBatch: async () => {
      // 测试进行中禁止重复触发，避免两轮同时抢槽位。
      if (get().batch.running) return;

      set({
        batch: { ...EMPTY_BATCH_STATE, running: true },
        testing: {},
        error: null,
      });

      let report: BatchReport;
      try {
        report = normalizeBatchReport(await persistence.startBatch());
      } catch (error) {
        set({
          batch: { ...EMPTY_BATCH_STATE },
          error: errorMessage(error),
        });
        return;
      }

      // 整轮结果覆盖到对应行：未测的行也写进去，界面才能区分「没测」与「失败」。
      const results = { ...get().results };
      for (const row of report.rows) {
        results[probeKey(row.host, row.port)] = {
          status: row.status,
          latencyMs: row.latencyMs,
          error: row.error,
        };
      }
      // 收尾后迟到的事件不再写入，最终结果以整轮报告为准。
      markFinished(report.runId);

      set({
        results,
        batch: {
          running: false,
          stopping: false,
          mode: "latency",
          runId: report.runId,
          completed: report.completed,
          total: report.total,
          stopped: report.stopped,
        },
        error: null,
      });
    },

    startSpeedBatch: async (targets) => {
      // 与延迟测试共用同一轮槽位，测试进行中禁止重复触发。
      if (get().batch.running) return;

      set({
        batch: { ...EMPTY_BATCH_STATE, running: true, mode: "speed" },
        error: null,
      });

      let report: SpeedReport;
      try {
        report = normalizeSpeedReport(await persistence.speedBatch(targets));
      } catch (error) {
        set({
          batch: { ...EMPTY_BATCH_STATE },
          error: errorMessage(error),
        });
        return;
      }

      // 只覆盖真正测过的行；停止后未轮到的行保留原速度。
      const speeds = { ...get().speeds };
      for (const row of report.rows) {
        if (row.status === "untested") continue;
        speeds[probeKey(row.host, row.port)] = {
          status: row.status,
          mbps: row.mbps,
          error: row.error,
        };
      }
      markFinished(report.runId);

      set({
        speeds,
        batch: {
          running: false,
          stopping: false,
          mode: "speed",
          runId: report.runId,
          completed: report.completed,
          total: report.total,
          stopped: report.stopped,
        },
        error: null,
      });
    },

    startGeoBatch: async (targets) => {
      // 与延迟、测速共用同一轮槽位，测试进行中禁止重复触发。
      if (get().batch.running) return;

      set({
        batch: { ...EMPTY_BATCH_STATE, running: true, mode: "geo" },
        error: null,
      });

      let report: GeoReport;
      try {
        report = normalizeGeoReport(await persistence.geoBatch(targets));
      } catch (error) {
        set({
          batch: { ...EMPTY_BATCH_STATE },
          error: errorMessage(error),
        });
        return;
      }

      // 只覆盖真正查到结论的行；失败或未测的行保留上一次的归属地。
      const geos = { ...get().geos };
      for (const row of report.rows) {
        if (row.status !== "ok") continue;
        geos[probeKey(row.host, row.port)] = {
          ip: row.ip,
          country: row.country,
          zh: row.zh,
          en: row.en,
          asn: row.asn,
          source: row.source,
        };
      }
      markFinished(report.runId);

      set({
        geos,
        batch: {
          running: false,
          stopping: false,
          mode: "geo",
          runId: report.runId,
          completed: report.completed,
          total: report.total,
          stopped: report.stopped,
        },
        error: null,
      });
    },

    cancelBatch: async () => {
      if (get().batch.running) {
        set({ batch: { ...get().batch, stopping: true } });
      }
      try {
        await persistence.cancelBatch();
      } catch (error) {
        set({ error: errorMessage(error) });
      }
    },

    applyProgress: (progress) => {
      set({
        batch: {
          ...get().batch,
          running: true,
          mode: progress.mode ?? get().batch.mode,
          runId: progress.runId,
          completed: progress.completed,
          total: progress.total,
        },
      });
    },

    applyRows: (rows) => {
      if (rows.length === 0) return;

      // 一次性复制三张表，写完再 set，避免每条事件都触发整表重排。
      const results = { ...get().results };
      const speeds = { ...get().speeds };
      const geos = { ...get().geos };
      let changed = false;

      for (const row of rows) {
        // 该轮已经收尾，迟到的事件直接丢弃，最终结果以报告为准。
        if (row.runId !== "" && finishedRuns.includes(row.runId)) continue;
        if (mergeProbeRow(row, results, speeds, geos)) changed = true;
      }

      if (changed) set({ results, speeds, geos });
    },
  }));
}

/** 生产环境走 Tauri 命令；浏览器里跑原型时直接给一条可读失败。 */
export const tauriProbePersistence: ProbePersistence = {
  probe: (host, port, alias) =>
    isTauriRuntime()
      ? invoke<unknown>("probe_single", { host, port, alias })
      : Promise.resolve({
          status: "fail",
          latencyMs: null,
          error: "当前不在 Tauri 窗口内，无法发起探测",
        }),
  startBatch: () =>
    isTauriRuntime()
      ? invoke<unknown>("probe_batch")
      : Promise.reject(new Error("当前不在 Tauri 窗口内，无法发起探测")),
  speedBatch: (targets) =>
    isTauriRuntime()
      ? invoke<unknown>("probe_speed_batch", { targets: targets ?? null })
      : Promise.reject(new Error("当前不在 Tauri 窗口内，无法发起探测")),
  geoBatch: (targets) =>
    isTauriRuntime()
      ? invoke<unknown>("probe_geo_batch", { targets: targets ?? null })
      : Promise.reject(new Error("当前不在 Tauri 窗口内，无法查询归属地")),
  cancelBatch: () =>
    isTauriRuntime() ? invoke<unknown>("probe_cancel") : Promise.resolve(false),
};

export const useProbeStore = createProbeStore(tauriProbePersistence);

/** 订阅后端推来的批量进度；返回取消订阅函数。 */
export async function subscribeProbeProgress(
  handler: (progress: BatchProgress) => void,
): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};
  return listen<BatchProgress>("probe:progress", (event) => {
    const payload = event.payload;
    if (!isRecord(payload)) return;
    handler({
      runId: str(payload.runId),
      mode:
        payload.mode === "speed"
          ? "speed"
          : payload.mode === "geo"
            ? "geo"
            : "latency",
      completed: count(payload.completed),
      total: count(payload.total),
    });
  });
}

/**
 * 订阅后端逐条推送的实时结果；内部按 [`PROBE_ROW_FLUSH_MS`] 合并同类结果，
 * 避免整表频繁重排。返回取消订阅函数，取消时丢弃尚未写出的结果。
 */
export async function subscribeProbeRow(
  handler: (rows: ProbeRowEvent[]) => void,
  intervalMs = PROBE_ROW_FLUSH_MS,
): Promise<() => void> {
  if (!isTauriRuntime()) return () => {};

  const queue = createProbeRowQueue(handler, intervalMs);
  const unlisten = await listen<unknown>("probe:row", (event) => {
    const row = normalizeProbeRow(event.payload);
    if (row) queue.push(row);
  });

  return () => {
    queue.clear();
    unlisten();
  };
}

export const selectProbeResults = (state: ProbeStore) => state.results;
export const selectProbeSpeeds = (state: ProbeStore) => state.speeds;
export const selectProbeGeos = (state: ProbeStore) => state.geos;
export const selectProbeTesting = (state: ProbeStore) => state.testing;
export const selectProbeError = (state: ProbeStore) => state.error;
export const selectProbeBatch = (state: ProbeStore) => state.batch;
