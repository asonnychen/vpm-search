import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage, isRecord, isTauriRuntime } from "./utils";

/** 表格列数，与 `ResultsTable` 的列一一对应，必须与 Rust 侧一致。 */
export const COLUMN_COUNT = 10;

export type LastInput = {
  /** 优选源 txt 地址，每行一个。 */
  sourceUrls: string;
  /** 手动补充的优选地址，换行或分号分隔。 */
  manualAddresses: string;
};

export type SpeedParams = {
  streams: number;
  sizeMb: number;
  timeoutS: number;
};

/** 请求总超时固定 10 秒，只读展示，不写进设置。 */
export const REQUEST_TIMEOUT_S = 10;
/** 每条地址的延迟采样固定 3 次，只读展示，不写进设置。 */
export const LATENCY_SAMPLES = 3;
/** 测速固定 4 流 · 10MB · 15 秒，只读展示，不写进设置。 */
export const SPEED_PARAMS: SpeedParams = {
  streams: 4,
  sizeMb: 10,
  timeoutS: 15,
};

export type LogRetention = {
  operationDays: number;
  appDays: number;
  /** 探测日志的保留天数；旧设置里的 singboxDays 已废弃。 */
  probeDays: number;
};

export type UiState = {
  logHeight: number;
  logCollapsed: boolean;
  configOpen: boolean;
  poolOpen: boolean;
  /** 表格里的「延时 ≤ N ms」过滤，null 表示不过滤。 */
  latencyFilterMs: number | null;
};

export type Settings = {
  lastInput: LastInput;
  maxLatencyMs: number;
  connectTimeoutS: number;
  concurrency: number;
  columnWidths: number[];
  logRetention: LogRetention;
  ui: UiState;
};

export const DEFAULT_COLUMN_WIDTHS = [64, 172, 140, 58, 84, 96, 156, 112, 88, 84];

/**
 * 地址并发默认值：逻辑核数 × 2，夹在 8–32 之间。
 * 多核机器才能把并发槽位铺满；用户在设置里改过的值优先。
 */
export function defaultConcurrency(
  cores = typeof navigator === "undefined" ? 1 : navigator.hardwareConcurrency || 1,
): number {
  return Math.min(32, Math.max(8, Math.round(cores) * 2));
}

export const DEFAULT_SETTINGS: Settings = {
  lastInput: { sourceUrls: "", manualAddresses: "" },
  maxLatencyMs: 200,
  connectTimeoutS: 5,
  concurrency: defaultConcurrency(),
  columnWidths: [...DEFAULT_COLUMN_WIDTHS],
  logRetention: { operationDays: 30, appDays: 30, probeDays: 30 },
  ui: {
    logHeight: 214,
    logCollapsed: false,
    configOpen: true,
    poolOpen: false,
    latencyFilterMs: null,
  },
};

/** 只接受区间内的有限数字，其余一律回落默认值，避免脏值进入界面。 */
function numberIn(value: unknown, fallback: number, min: number, max: number): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  if (value < min || value > max) return fallback;
  return Math.round(value);
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function normalizeWidths(value: unknown): number[] {
  if (!Array.isArray(value) || value.length !== COLUMN_COUNT) {
    return [...DEFAULT_COLUMN_WIDTHS];
  }
  const widths = value.map((w) => numberIn(w, Number.NaN, 40, 2000));
  if (widths.some((w) => Number.isNaN(w))) return [...DEFAULT_COLUMN_WIDTHS];
  return widths;
}

/**
 * 把磁盘上读到的任意 JSON 归一成一份完整设置：
 * 每个字段独立校验，坏字段只回落自己的默认值。
 */
export function normalizeSettings(raw: unknown): Settings {
  const source = isRecord(raw) ? raw : {};
  const lastInput = isRecord(source.lastInput) ? source.lastInput : {};
  const retention = isRecord(source.logRetention) ? source.logRetention : {};
  const ui = isRecord(source.ui) ? source.ui : {};
  // 请求总超时、延迟采样与测速参数是固定契约，界面上只读展示常量；
  // 旧设置里即使存了别的值也直接忽略，避免出现与契约矛盾的显示值。

  return {
    lastInput: {
      sourceUrls: text(lastInput.sourceUrls, DEFAULT_SETTINGS.lastInput.sourceUrls),
      manualAddresses: text(
        lastInput.manualAddresses,
        DEFAULT_SETTINGS.lastInput.manualAddresses,
      ),
    },
    maxLatencyMs: numberIn(source.maxLatencyMs, DEFAULT_SETTINGS.maxLatencyMs, 1, 10_000),
    connectTimeoutS: numberIn(source.connectTimeoutS, DEFAULT_SETTINGS.connectTimeoutS, 1, 10),
    concurrency: numberIn(source.concurrency, DEFAULT_SETTINGS.concurrency, 1, 32),
    columnWidths: normalizeWidths(source.columnWidths),
    logRetention: {
      operationDays: numberIn(
        retention.operationDays,
        DEFAULT_SETTINGS.logRetention.operationDays,
        1,
        3650,
      ),
      appDays: numberIn(retention.appDays, DEFAULT_SETTINGS.logRetention.appDays, 1, 3650),
      probeDays: numberIn(retention.probeDays, DEFAULT_SETTINGS.logRetention.probeDays, 1, 3650),
    },
    ui: {
      logHeight: numberIn(ui.logHeight, DEFAULT_SETTINGS.ui.logHeight, 80, 4000),
      logCollapsed: bool(ui.logCollapsed, DEFAULT_SETTINGS.ui.logCollapsed),
      configOpen: bool(ui.configOpen, DEFAULT_SETTINGS.ui.configOpen),
      poolOpen: bool(ui.poolOpen, DEFAULT_SETTINGS.ui.poolOpen),
      latencyFilterMs:
        typeof ui.latencyFilterMs === "number" && Number.isFinite(ui.latencyFilterMs)
          ? Math.min(60_000, Math.max(1, Math.round(ui.latencyFilterMs)))
          : null,
    },
  };
}

/** 持久化端口：默认接 Tauri 命令，测试里换成假实现。 */
export type SettingsPersistence = {
  load: () => Promise<unknown>;
  save: (settings: Settings) => Promise<void>;
};

export type SettingsPatch = Omit<
  Partial<Settings>,
  "lastInput" | "logRetention" | "ui"
> & {
  lastInput?: Partial<LastInput>;
  logRetention?: Partial<LogRetention>;
  ui?: Partial<UiState>;
};

export type SettingsStore = {
  settings: Settings;
  loaded: boolean;
  saveError: string | null;
  load: () => Promise<void>;
  patch: (patch: SettingsPatch) => void;
  setColumnWidth: (index: number, width: number) => void;
  flush: () => Promise<void>;
};

export function createSettingsStore(
  persistence: SettingsPersistence,
  options: { debounceMs?: number } = {},
) {
  const debounceMs = options.debounceMs ?? 400;
  let timer: ReturnType<typeof setTimeout> | null = null;

  return create<SettingsStore>()((set, get) => {
    const persist = async () => {
      try {
        await persistence.save(get().settings);
        set({ saveError: null });
      } catch (error) {
        // 写失败只在界面上留一条记录，不让界面崩溃。
        set({ saveError: errorMessage(error) });
      }
    };

    const schedule = () => {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void persist();
      }, debounceMs);
    };

    return {
      settings: normalizeSettings(null),
      loaded: false,
      saveError: null,

      load: async () => {
        try {
          const raw = await persistence.load();
          set({ settings: normalizeSettings(raw), loaded: true, saveError: null });
        } catch (error) {
          // 读失败保留默认值，界面照常可用。
          set({ loaded: true, saveError: errorMessage(error) });
        }
      },

      patch: (patch) => {
        const current = get().settings;
        const next: Settings = {
          ...current,
          ...patch,
          lastInput: { ...current.lastInput, ...patch.lastInput },
          logRetention: { ...current.logRetention, ...patch.logRetention },
          ui: { ...current.ui, ...patch.ui },
        };
        set({ settings: normalizeSettings(next) });
        schedule();
      },

      setColumnWidth: (index, width) => {
        const current = get().settings;
        if (index < 0 || index >= current.columnWidths.length) return;
        const widths = [...current.columnWidths];
        widths[index] = Math.min(2000, Math.max(40, Math.round(width)));
        set({ settings: { ...current, columnWidths: widths } });
        schedule();
      },

      flush: async () => {
        if (timer !== null) {
          clearTimeout(timer);
          timer = null;
        }
        await persist();
      },
    };
  });
}

/** 生产环境走 Tauri 命令；浏览器里跑原型时只保留默认值。 */
export const tauriPersistence: SettingsPersistence = {
  load: () => (isTauriRuntime() ? invoke<unknown>("get_settings") : Promise.resolve(null)),
  save: (settings) =>
    isTauriRuntime()
      ? invoke<void>("save_settings", { settings }).then(() => undefined)
      : Promise.resolve(),
};

export const useSettingsStore = createSettingsStore(tauriPersistence);

export const selectSettings = (state: SettingsStore) => state.settings;
export const selectThresholdMs = (state: SettingsStore) => state.settings.maxLatencyMs;
export const selectColumnWidths = (state: SettingsStore) => state.settings.columnWidths;
export const selectLastInput = (state: SettingsStore) => state.settings.lastInput;
export const selectLogRetention = (state: SettingsStore) => state.settings.logRetention;
export const selectUi = (state: SettingsStore) => state.settings.ui;
