import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_SETTINGS,
  createSettingsStore,
  normalizeSettings,
  selectColumnWidths,
  selectLastInput,
  selectLogRetention,
  selectNodeTemplates,
  selectSpeedParams,
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
      nodeTemplates: "不是数组",
      logRetention: { appDays: 0 },
    });
    expect(settings.maxLatencyMs).toBe(DEFAULT_SETTINGS.maxLatencyMs);
    expect(settings.concurrency).toBe(DEFAULT_SETTINGS.concurrency);
    expect(settings.columnWidths).toEqual(DEFAULT_SETTINGS.columnWidths);
    expect(settings.nodeTemplates).toEqual(DEFAULT_SETTINGS.nodeTemplates);
    expect(settings.logRetention.appDays).toBe(DEFAULT_SETTINGS.logRetention.appDays);
    expect(settings.logRetention.operationDays).toBe(DEFAULT_SETTINGS.logRetention.operationDays);
  });

  it("完全无法识别的输入返回默认设置", () => {
    expect(normalizeSettings(null)).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings("乱码")).toEqual(DEFAULT_SETTINGS);
  });
});

describe("设置 store 的读写", () => {
  it("启动读取后回填选择器", async () => {
    const persistence = fakePersistence({
      load: async () => ({
        maxLatencyMs: 350,
        columnWidths: Array(14).fill(100),
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
    expect(selectColumnWidths(store.getState())).toHaveLength(14);
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
          speed: { streams: 6, sizeMb: 20, timeoutS: 25 },
          logRetention: { operationDays: 7, appDays: 14, singboxDays: 60 },
          nodeTemplates: [{ id: "t1", name: "备用", kind: "trojan", transport: "ws", tls: true }],
        }),
      }),
    );
    await store.getState().load();

    const state = store.getState();
    expect(selectSpeedParams(state)).toEqual({ streams: 6, sizeMb: 20, timeoutS: 25 });
    expect(selectLogRetention(state)).toEqual({ operationDays: 7, appDays: 14, singboxDays: 60 });
    expect(selectNodeTemplates(state)).toHaveLength(1);
    expect(selectNodeTemplates(state)[0].name).toBe("备用");
  });
});
