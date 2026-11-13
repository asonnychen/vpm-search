import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import {
  normalizeProbeResult,
  type ProbeStatus,
  type SpeedStatus,
} from "./probe";

/** 数据库体积提示阈值：超过 50MB 就建议按轮次清理。 */
export const HISTORY_DB_LIMIT_BYTES = 50 * 1024 * 1024;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function nullableStr(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function num(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function count(value: unknown): number {
  return Math.max(0, Math.round(num(value)));
}

function nullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** 历史入口的一轮测试：时间、参数快照与统计。 */
export type TestRunSummary = {
  id: string;
  startedAt: string;
  finishedAt: string | null;
  /** 当时的参数快照，字段随测试类型变化。 */
  params: Record<string, unknown>;
  total: number;
  ok: number;
  timeout: number;
  fail: number;
  /** 本轮实际落库的地址数。 */
  addressCount: number;
};

/** 历史结果的一行，字段与主表格的测量列一致。 */
export type HistoryResult = {
  host: string;
  port: number;
  alias: string;
  status: ProbeStatus;
  latencyMs: number | null;
  error: string | null;
  speedMbps: number | null;
  speedStatus: SpeedStatus;
  speedError: string | null;
  exitIp: string | null;
  country: string | null;
  zh: string;
  en: string;
  colo: string | null;
};

/** 某一轮的全部结果。 */
export type HistoryDetail = {
  run: TestRunSummary | null;
  results: HistoryResult[];
};

/** 同一地址在某一轮的结果点。 */
export type AddressPoint = {
  runId: string;
  startedAt: string;
  status: ProbeStatus;
  latencyMs: number | null;
  speedMbps: number | null;
  speedStatus: SpeedStatus;
};

/** 跨轮对比的一行：数值 + 相对最大值的百分比（CSS 条形用）。 */
export type ComparisonRow = AddressPoint & {
  latencyPct: number;
  speedPct: number;
};

/** 把后端返回的一轮归一；坏数据只丢自己。 */
export function normalizeTestRun(raw: unknown): TestRunSummary | null {
  if (!isRecord(raw)) return null;
  const id = str(raw.id).trim();
  const startedAt = str(raw.startedAt).trim();
  if (id === "" || startedAt === "") return null;
  return {
    id,
    startedAt,
    finishedAt: nullableStr(raw.finishedAt),
    params: isRecord(raw.params) ? raw.params : {},
    total: count(raw.total),
    ok: count(raw.ok),
    timeout: count(raw.timeout),
    fail: count(raw.fail),
    addressCount: count(raw.addressCount),
  };
}

/** 轮次列表按开始时间倒序，最近的在前。 */
export function normalizeTestRuns(raw: unknown): TestRunSummary[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .map(normalizeTestRun)
    .filter((run): run is TestRunSummary => run !== null)
    .sort((a, b) => b.startedAt.localeCompare(a.startedAt));
}

/** 把历史结果行归一；成功却没有延迟 / 速度按失败处理。 */
export function normalizeHistoryResult(raw: unknown): HistoryResult | null {
  if (!isRecord(raw)) return null;
  const host = str(raw.host).trim();
  const port = Math.round(num(raw.port));
  if (host === "" || port <= 0) return null;

  const latency = normalizeProbeResult(raw);
  const speedStatusRaw = str(raw.speedStatus, "untested");
  const mbps = nullableNumber(raw.speedMbps);
  let speedStatus: SpeedStatus;
  let speedMbps: number | null = null;
  if (speedStatusRaw === "untested") {
    speedStatus = "untested";
  } else if (speedStatusRaw === "ok" && mbps !== null && mbps > 0) {
    speedStatus = "ok";
    speedMbps = mbps;
  } else {
    speedStatus = "fail";
  }

  return {
    host,
    port,
    alias: str(raw.alias),
    status: latency.status,
    latencyMs: latency.latencyMs,
    error: latency.error,
    speedMbps,
    speedStatus,
    speedError: speedStatus === "fail" ? nullableStr(raw.speedError) : null,
    exitIp: nullableStr(raw.exitIp),
    country: nullableStr(raw.country),
    zh: str(raw.zh),
    en: str(raw.en),
    colo: nullableStr(raw.colo),
  };
}

export function normalizeHistoryResults(raw: unknown): HistoryResult[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .map(normalizeHistoryResult)
    .filter((row): row is HistoryResult => row !== null);
}

/** 把后端返回的「某一轮结果」归一。 */
export function normalizeHistoryDetail(raw: unknown): HistoryDetail {
  if (!isRecord(raw)) return { run: null, results: [] };
  return {
    run: normalizeTestRun(raw.run),
    results: normalizeHistoryResults(raw.results),
  };
}

/** 同一地址跨轮次的结果点，按时间升序；空 runId 的坏点丢弃。 */
export function normalizeAddressHistory(raw: unknown): AddressPoint[] {
  const list = isRecord(raw) && Array.isArray(raw.points) ? raw.points : [];
  return list
    .filter(isRecord)
    .map((item) => {
      const runId = str(item.runId).trim();
      const startedAt = str(item.startedAt).trim();
      if (runId === "" || startedAt === "") return null;

      const latency = normalizeProbeResult(item);
      const speedStatusRaw = str(item.speedStatus, "untested");
      const mbps = nullableNumber(item.speedMbps);
      let speedStatus: SpeedStatus = "untested";
      let speedMbps: number | null = null;
      if (speedStatusRaw === "ok" && mbps !== null && mbps > 0) {
        speedStatus = "ok";
        speedMbps = mbps;
      } else if (speedStatusRaw === "fail") {
        speedStatus = "fail";
      }

      return {
        runId,
        startedAt,
        status: latency.status,
        latencyMs: latency.latencyMs,
        speedMbps,
        speedStatus,
      } satisfies AddressPoint;
    })
    .filter((point): point is AddressPoint => point !== null)
    .sort((a, b) => a.startedAt.localeCompare(b.startedAt));
}

/** 把跨轮序列换算成对比条百分比：各自以最大值为 100，空值 0。 */
export function buildComparison(points: AddressPoint[]): ComparisonRow[] {
  const maxLatency = Math.max(
    0,
    ...points.map((point) => point.latencyMs ?? 0),
  );
  const maxSpeed = Math.max(0, ...points.map((point) => point.speedMbps ?? 0));

  return points.map((point) => ({
    ...point,
    latencyPct:
      point.latencyMs !== null && maxLatency > 0
        ? Math.round((point.latencyMs / maxLatency) * 1000) / 10
        : 0,
    speedPct:
      point.speedMbps !== null && maxSpeed > 0
        ? Math.round((point.speedMbps / maxSpeed) * 1000) / 10
        : 0,
  }));
}

/** 数据库体积的可读文本。 */
export function formatBytes(bytes: number): string {
  const size = Math.max(0, num(bytes));
  if (size < 1024) return `${Math.round(size)} B`;
  if (size < 1024 ** 2) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 ** 3) return `${(size / 1024 ** 2).toFixed(1)} MB`;
  return `${(size / 1024 ** 3).toFixed(1)} GB`;
}

/** 是否超过 50MB 清理阈值。 */
export function isOverDbLimit(bytes: number): boolean {
  return num(bytes) >= HISTORY_DB_LIMIT_BYTES;
}

/** 一轮的参数快照折成一行可读文字。 */
export function describeParams(run: TestRunSummary): string {
  const parts: string[] = [];
  const kind = str(run.params.kind);
  const transport = str(run.params.transport);
  if (kind) parts.push(transport ? `${kind}/${transport}` : kind);
  if (typeof run.params.tls === "boolean") parts.push(run.params.tls ? "TLS" : "无 TLS");
  const samples = num(run.params.samples, 0);
  if (samples > 0) parts.push(`采样 ${samples} 次`);
  const concurrency = num(run.params.concurrency, 0);
  if (concurrency > 0) parts.push(`并发 ${concurrency}`);
  const streams = num(run.params.streams, 0);
  if (streams > 0) parts.push(`${streams} 流`);
  const sizeMb = num(run.params.sizeMb, 0);
  if (sizeMb > 0) parts.push(`${sizeMb}MB`);
  const mode = str(run.params.mode);
  if (mode) parts.push(mode === "speed" ? "测速" : mode === "geo" ? "归属地" : mode);
  return parts.length > 0 ? parts.join(" · ") : "参数快照为空";
}

/** 持久化端口：默认接 Tauri 命令，测试里换成假实现。 */
export type HistoryPersistence = {
  listRuns: () => Promise<unknown>;
  getRunResults: (runId: string) => Promise<unknown>;
  deleteRun: (runId: string) => Promise<unknown>;
  databaseSize: () => Promise<unknown>;
  addressHistory: (host: string, port: number) => Promise<unknown>;
};

export type HistoryStore = {
  /** 历史弹窗是否打开。 */
  open: boolean;
  loading: boolean;
  runs: TestRunSummary[];
  selectedRunId: string | null;
  detail: HistoryDetail | null;
  detailLoading: boolean;
  /** 当前对比的地址。 */
  address: { host: string; port: number; points: AddressPoint[] } | null;
  addressLoading: boolean;
  dbBytes: number;
  deleting: string | null;
  error: string | null;
  setOpen: (open: boolean) => void;
  openPanel: () => Promise<void>;
  closePanel: () => void;
  refresh: () => Promise<void>;
  loadSize: () => Promise<void>;
  selectRun: (runId: string) => Promise<void>;
  deleteRun: (runId: string) => Promise<void>;
  loadAddress: (host: string, port: number) => Promise<void>;
  clearAddress: () => void;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createHistoryStore(persistence: HistoryPersistence) {
  return create<HistoryStore>()((set, get) => ({
    open: false,
    loading: false,
    runs: [],
    selectedRunId: null,
    detail: null,
    detailLoading: false,
    address: null,
    addressLoading: false,
    dbBytes: 0,
    deleting: null,
    error: null,

    setOpen: (open) => set({ open }),

    openPanel: async () => {
      set({ open: true });
      await get().refresh();
    },

    closePanel: () => set({ open: false }),

    refresh: async () => {
      set({ loading: true, error: null });
      try {
        const [runsRaw, sizeRaw] = await Promise.all([
          persistence.listRuns(),
          persistence.databaseSize(),
        ]);
        set({
          runs: normalizeTestRuns(runsRaw),
          dbBytes: Math.max(0, Math.round(num(sizeRaw))),
          loading: false,
        });
      } catch (error) {
        set({ loading: false, error: errorMessage(error) });
      }
    },

    loadSize: async () => {
      try {
        set({ dbBytes: Math.max(0, Math.round(num(await persistence.databaseSize()))) });
      } catch {
        // 体积读不到不影响主界面，保留上一次的值。
      }
    },

    selectRun: async (runId) => {
      set({ selectedRunId: runId, detail: null, detailLoading: true, error: null });
      try {
        const detail = normalizeHistoryDetail(await persistence.getRunResults(runId));
        // 用户可能在加载期间切到别的轮次，只接受当前选中的结果。
        if (get().selectedRunId !== runId) return;
        set({ detail, detailLoading: false });
      } catch (error) {
        if (get().selectedRunId !== runId) return;
        set({ detailLoading: false, error: errorMessage(error) });
      }
    },

    deleteRun: async (runId) => {
      set({ deleting: runId, error: null });
      try {
        await persistence.deleteRun(runId);
        const wasSelected = get().selectedRunId === runId;
        set({
          deleting: null,
          runs: get().runs.filter((run) => run.id !== runId),
          ...(wasSelected ? { selectedRunId: null, detail: null } : {}),
        });
        await get().refresh();
      } catch (error) {
        set({ deleting: null, error: errorMessage(error) });
      }
    },

    loadAddress: async (host, port) => {
      set({ addressLoading: true, error: null });
      try {
        const points = normalizeAddressHistory(
          await persistence.addressHistory(host, port),
        );
        set({ address: { host, port, points }, addressLoading: false });
      } catch (error) {
        set({ addressLoading: false, error: errorMessage(error) });
      }
    },

    clearAddress: () => set({ address: null }),
  }));
}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 生产环境走 Tauri 命令；浏览器原型里返回空数据。 */
export const tauriHistoryPersistence: HistoryPersistence = {
  listRuns: () =>
    isTauriRuntime() ? invoke<unknown>("list_test_runs") : Promise.resolve([]),
  getRunResults: (runId) =>
    isTauriRuntime()
      ? invoke<unknown>("get_test_run_results", { runId })
      : Promise.resolve(null),
  deleteRun: (runId) =>
    isTauriRuntime()
      ? invoke<unknown>("delete_test_run", { runId })
      : Promise.resolve(false),
  databaseSize: () =>
    isTauriRuntime() ? invoke<unknown>("get_database_size") : Promise.resolve(0),
  addressHistory: (host, port) =>
    isTauriRuntime()
      ? invoke<unknown>("get_address_history", { host, port })
      : Promise.resolve({ host, port, points: [] }),
};

export const useHistoryStore = createHistoryStore(tauriHistoryPersistence);

export const selectHistoryOpen = (state: HistoryStore) => state.open;
export const selectHistoryRuns = (state: HistoryStore) => state.runs;
export const selectHistoryDbBytes = (state: HistoryStore) => state.dbBytes;
