import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_SETTINGS,
  LATENCY_SAMPLES,
  REQUEST_TIMEOUT_S,
  SPEED_PARAMS,
  createSettingsStore,
  defaultConcurrency,
  normalizeSettings,
  selectColumnWidths,
  selectLastInput,
  selectLogRetention,
  selectThresholdMs,
  type Settings,
  type SettingsPersistence,
} from "./settings";

function fakePersistence(overrides: Partial<SettingsPersistence> = {}): SettingsPersistence {
  return {
    load: vi.fn(async () => null),
    save: vi.fn(async () => {}),
    ...overrides,
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("normalizeSettings", () => {
  it("默认并发按核数放大并夹在 8–32", () => {
    expect(defaultConcurrency(1)).toBe(8);
    expect(defaultConcurrency(4)).toBe(8);
    expect(defaultConcurrency(8)).toBe(16);
    expect(defaultConcurrency(32)).toBe(32);
    expect(defaultConcurrency(64)).toBe(32);
    expect(DEFAULT_SETTINGS.concurrency).toBeGreaterThanOrEqual(8);
    expect(DEFAULT_SETTINGS.concurrency).toBeLessThanOrEqual(32);
  });

  it("用户设置里的并发值优先，不被默认值覆盖", () => {
    expect(normalizeSettings({ concurrency: 6 }).concurrency).toBe(6);
    expect(normalizeSettings({ concurrency: 32 }).concurrency).toBe(32);
  });

  it("缺字段时补齐默认值", () => {
    const settings = normalizeSettings({ maxLatencyMs: 350 });
    expect(settings.maxLatencyMs).toBe(350);
    expect(settings.lastInput).toEqual(DEFAULT_SETTINGS.lastInput);
    expect(settings.columnWidths).toEqual(DEFAULT_SETTINGS.columnWidths);
  });

  it("非法值回落默认值而不是原样带进界面", () => {
    const settings = normalizeSettings({
      maxLatencyMs: -1,
      concurrency: 999,
      columnWidths: [1, 2],
      logRetention: { appDays: 0 },
    });
    expect(settings.maxLatencyMs).toBe(DEFAULT_SETTINGS.maxLatencyMs);
    expect(settings.concurrency).toBe(DEFAULT_SETTINGS.concurrency);
    expect(settings.columnWidths).toEqual(DEFAULT_SETTINGS.columnWidths);
    expect(settings.logRetention.appDays).toBe(DEFAULT_SETTINGS.logRetention.appDays);
    expect(settings.logRetention.operationDays).toBe(DEFAULT_SETTINGS.logRetention.operationDays);
  });

  it("完全无法识别的输入返回默认设置", () => {
    expect(normalizeSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings("乱码")).toEqual(DEFAULT_SETTINGS);
  });

  it("旧的只读参数被忽略，显示值只能来自固定契约", () => {
    const settings = normalizeSettings({
      requestTimeoutS: 99,
      latencySamples: 9,
      speed: { streams: 16, sizeMb: 512, timeoutS: 300 },
    });
    expect(settings).not.toHaveProperty("requestTimeoutS");
    expect(settings).not.toHaveProperty("latencySamples");
    expect(settings).not.toHaveProperty("speed");
    expect(REQUEST_TIMEOUT_S).toBe(10);
    expect(LATENCY_SAMPLES).toBe(3);
    expect(SPEED_PARAMS).toEqual({ streams: 4, sizeMb: 10, timeoutS: 15 });
  });
});

describe("设置 store 的读写", () => {
  it("启动读取后回填选择器", async () => {
    const persistence = fakePersistence({
      load: async () => ({
        maxLatencyMs: 350,
        columnWidths: Array(10).fill(100),
        lastInput: { sourceUrls: "https://example.com/all.txt" },
      }),
    });
    const store = createSettingsStore(persistence);

    await store.getState().load();

    const state = store.getState();
    expect(state.loaded).toBe(true);
    expect(selectThresholdMs(state)).toBe(350);
    expect(selectColumnWidths(state)[0]).toBe(100);
    expect(selectLastInput(state).sourceUrls).toBe("https://example.com/all.txt");
    expect(selectLastInput(state).manualAddresses).toBe("");
  });

  it("修改后防抖保存整份设置", async () => {
    vi.useFakeTimers();
    const save = vi.fn(async (_settings: Settings) => {});
    const store = createSettingsStore(fakePersistence({ save }), { debounceMs: 400 });

    store.getState().patch({ concurrency: 12 });
    store.getState().patch({ concurrency: 16 });
    expect(save).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(400);
    expect(save).toHaveBeenCalledTimes(1);
    const saved = save.mock.calls[0]![0];
    expect(saved.concurrency).toBe(16);
    expect(saved.columnWidths).toEqual(DEFAULT_SETTINGS.columnWidths);
  });

  it("拖拽列宽只改对应列并持久化", async () => {
    vi.useFakeTimers();
    const save = vi.fn(async (_settings: Settings) => {});
    const store = createSettingsStore(fakePersistence({ save }), { debounceMs: 100 });

    store.getState().setColumnWidth(2, 222.4);
    expect(selectColumnWidths(store.getState())[2]).toBe(222);
    expect(selectColumnWidths(store.getState())[0]).toBe(DEFAULT_SETTINGS.columnWidths[0]);

    await vi.advanceTimersByTimeAsync(100);
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("列宽越界或过窄时被约束或忽略", () => {
    const store = createSettingsStore(fakePersistence());
    store.getState().setColumnWidth(0, 5);
    expect(selectColumnWidths(store.getState())[0]).toBe(40);
    store.getState().setColumnWidth(99, 300);
    expect(selectColumnWidths(store.getState())).toHaveLength(10);
  });

  it("读写失败不抛异常，只记录 saveError", async () => {
    const store = createSettingsStore(
      fakePersistence({
        load: async () => {
          throw new Error("db down");
        },
        save: async () => {
          throw new Error("写失败");
        },
      }),
    );

    await expect(store.getState().load()).resolves.toBeUndefined();
    expect(store.getState().loaded).toBe(true);
    expect(store.getState().saveError).toContain("db down");

    await expect(store.getState().flush()).resolves.toBeUndefined();
    expect(store.getState().saveError).toContain("写失败");
  });

  it("选择器读出其余配置项", async () => {
    const store = createSettingsStore(
      fakePersistence({
        load: async () => ({
          logRetention: { operationDays: 7, appDays: 14, probeDays: 60 },
        }),
      }),
    );
    await store.getState().load();

    const state = store.getState();
    expect(selectLogRetention(state)).toEqual({ operationDays: 7, appDays: 14, probeDays: 60 });
  });
});
