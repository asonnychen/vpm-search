import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

/** 单条测试的三种结论，与后端 `ProbeStatus` 一致。 */
export type ProbeStatus = "ok" | "timeout" | "fail";

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

/** 持久化端口：默认接 Tauri 命令，测试里换成假实现。 */
export type ProbePersistence = {
  probe: (host: string, port: number, alias: string) => Promise<unknown>;
};

export type ProbeStore = {
  /** 按 `host:port` 索引的结果，未测的行不在表里。 */
  results: Record<string, ProbeResult>;
  /** 正在测试的行。 */
  testing: Record<string, boolean>;
  /** 最近一次失败原因，用于提示 sidecar 日志路径。 */
  error: string | null;
  testOne: (host: string, port: number, alias?: string) => Promise<ProbeResult>;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function createProbeStore(persistence: ProbePersistence) {
  return create<ProbeStore>()((set, get) => ({
    results: {},
    testing: {},
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
};

export const useProbeStore = createProbeStore(tauriProbePersistence);

export const selectProbeResults = (state: ProbeStore) => state.results;
export const selectProbeTesting = (state: ProbeStore) => state.testing;
export const selectProbeError = (state: ProbeStore) => state.error;
