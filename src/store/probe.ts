import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 单条测试的结论，与后端 `ProbeStatus` 一致。 */
export type ProbeStatus = "ok" | "timeout" | "fail" | "untested";

/** 一条地址的测试结果。 */
export type ProbeResult = {
  status: ProbeStatus;
  /** 成功时的往返毫秒数。 */
  latencyMs: number | null;
  /** 失败或超时的原因；sidecar 崩溃时含日志路径。 */
  error: string | null;
};

/** 表格行的键，与 `ResultsTable` 一致。 */
export function probeKey(host: string, port: number): string {
  return `${host}:${port}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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
  /** 经代理观测到的出口 IP，不是优选地址本身。 */
  exitIp: string | null;
  /** ISO 国家码；Cloudflare 落地为空。 */
  country: string | null;
  /** 归属地中文名。 */
  zh: string;
  /** 归属地英文名。 */
  en: string;
  /** Cloudflare 机房三字码；非 CF 落地或没有 colo 时为 null。 */
  colo: string | null;
  asn: number | null;
  /** 数据来源：offline / online / cache / trace。 */
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
            exitIp: null, country: null, zh: "", en: "", colo: null, asn: null, source: "" };
        }
        if (item.status !== "ok" || (zh === "" && en === "")) {
          return { host, port, alias, status: "fail" as const,
            error: error ?? "归属地查询失败",
            exitIp: null, country: null, zh: "", en: "", colo: null, asn: null, source: "" };
        }

        const asn = num(item.asn, 0);
        const colo = str(item.colo).trim();
        return {
          host,
          port,
          alias,
          status: "ok" as const,
          error: null,
          exitIp: str(item.exitIp) || null,
          country: str(item.country) || null,
          zh,
          en,
          // 无机房信息时第三段留空，不写占位符。
          colo: colo === "" ? null : colo,
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
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createProbeStore(persistence: ProbePersistence) {
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
          exitIp: row.exitIp,
          country: row.country,
          zh: row.zh,
          en: row.en,
          colo: row.colo,
          asn: row.asn,
          source: row.source,
        };
      }

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
  }));
}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 生产环境走 Tauri 命令；浏览器里跑原型时直接给一条可读失败。 */
export const tauriProbePersistence: ProbePersistence = {
  probe: (host, port, alias) =>
    isTauriRuntime()
      ? invoke<unknown>("probe_single", { host, port, alias })
      : Promise.resolve({
          status: "fail",
          latencyMs: null,
          error: "当前不在 Tauri 窗口内，无法启动 sing-box",
        }),
  startBatch: () =>
    isTauriRuntime()
      ? invoke<unknown>("probe_batch")
      : Promise.reject(new Error("当前不在 Tauri 窗口内，无法启动 sing-box")),
  speedBatch: (targets) =>
    isTauriRuntime()
      ? invoke<unknown>("probe_speed_batch", { targets: targets ?? null })
      : Promise.reject(new Error("当前不在 Tauri 窗口内，无法启动 sing-box")),
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

export const selectProbeResults = (state: ProbeStore) => state.results;
export const selectProbeSpeeds = (state: ProbeStore) => state.speeds;
export const selectProbeGeos = (state: ProbeStore) => state.geos;
export const selectProbeTesting = (state: ProbeStore) => state.testing;
export const selectProbeError = (state: ProbeStore) => state.error;
export const selectProbeBatch = (state: ProbeStore) => state.batch;
