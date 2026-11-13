import { describe, expect, it, vi } from "vitest";
import {
  createProbeStore,
  normalizeBatchReport,
  normalizeProbeResult,
  probeKey,
  type ProbePersistence,
} from "./probe";

function fakePersistence(
  overrides: Partial<ProbePersistence> = {},
): ProbePersistence {
  return {
    probe: vi.fn(async () => ({
      status: "ok",
      latencyMs: 120,
      error: null,
    })),
    startBatch: vi.fn(async () => ({
      runId: "run-1",
      total: 2,
      completed: 2,
      stopped: false,
      counts: { ok: 1, timeout: 1, fail: 0, untested: 0 },
      rows: [
        { host: "104.16.1.1", port: 8443, alias: "", status: "ok", latencyMs: 80, error: null },
        { host: "104.16.2.2", port: 8443, alias: "", status: "timeout", latencyMs: null, error: "超时" },
      ],
    })),
    cancelBatch: vi.fn(async () => true),
    ...overrides,
  };
}

describe("探测结果归一", () => {
  it("成功结果保留延迟并取整", () => {
    expect(normalizeProbeResult({ status: "ok", latencyMs: 120.6, error: null })).toEqual({
      status: "ok",
      latencyMs: 121,
      error: null,
    });
  });

  it("超时与失败不带延迟", () => {
    expect(normalizeProbeResult({ status: "timeout", error: "10s 内未收到响应" })).toEqual({
      status: "timeout",
      latencyMs: null,
      error: "10s 内未收到响应",
    });
    expect(normalizeProbeResult({ status: "fail", error: "连接被拒绝" }).status).toBe("fail");
  });

  it("未知状态或成功却没有延迟时回落成失败", () => {
    expect(normalizeProbeResult({ status: "乱写" }).status).toBe("fail");
    expect(normalizeProbeResult({ status: "ok" }).status).toBe("fail");
    expect(normalizeProbeResult(null).status).toBe("fail");
  });

  it("行键是 host:port", () => {
    expect(probeKey("104.16.1.1", 8443)).toBe("104.16.1.1:8443");
  });
});

describe("单条测试", () => {
  it("成功结果只写对应行", async () => {
    const persistence = fakePersistence();
    const store = createProbeStore(persistence);

    const result = await store.getState().testOne("104.16.1.1", 8443, "别名");

    expect(result?.status).toBe("ok");
    expect(store.getState().results["104.16.1.1:8443"]).toEqual({
      status: "ok",
      latencyMs: 120,
      error: null,
    });
    // 其余行保持未测。
    expect(store.getState().results["104.16.2.2:2053"]).toBeUndefined();
    expect(persistence.probe).toHaveBeenCalledWith("104.16.1.1", 8443, "别名");
  });

  it("失败时保留原因并让该行显示失败", async () => {
    const persistence = fakePersistence({
      probe: vi.fn(async () => ({
        status: "fail",
        latencyMs: null,
        error: "sing-box 已退出（exit code: 1），日志见 C:\\logs\\singbox.log",
      })),
    });
    const store = createProbeStore(persistence);

    await store.getState().testOne("104.16.1.1", 8443);

    const result = store.getState().results["104.16.1.1:8443"];
    expect(result.status).toBe("fail");
    expect(result.latencyMs).toBeNull();
    expect(result.error).toContain("singbox.log");
    // sidecar 崩溃要能提示日志路径。
    expect(store.getState().error).toContain("singbox.log");
  });

  it("测试过程中标记该行，结束后清除", async () => {
    let release: (value: unknown) => void = () => {};
    const pending = new Promise((resolve) => {
      release = resolve;
    });
    const store = createProbeStore(fakePersistence({ probe: () => pending }));

    const running = store.getState().testOne("104.16.1.1", 8443);
    expect(store.getState().testing["104.16.1.1:8443"]).toBe(true);

    release({ status: "ok", latencyMs: 88 });
    await running;

    expect(store.getState().testing["104.16.1.1:8443"]).toBeUndefined();
  });

  it("命令抛错时该行标失败而不是让界面崩溃", async () => {
    const store = createProbeStore(
      fakePersistence({
        probe: vi.fn(async () => {
          throw new Error("数据库不可用");
        }),
      }),
    );

    const result = await store.getState().testOne("104.16.1.1", 8443);

    expect(result?.status).toBe("fail");
    expect(result?.error).toContain("数据库不可用");
    expect(store.getState().error).toContain("数据库不可用");
  });
});

describe("批量测试", () => {
  it("未测结果原样保留，不伪装成失败", () => {
    expect(normalizeProbeResult({ status: "untested" })).toEqual({
      status: "untested",
      latencyMs: null,
      error: null,
    });
  });

  it("整轮结束后按 host:port 写入每一行", async () => {
    const persistence = fakePersistence();
    const store = createProbeStore(persistence);

    await store.getState().startBatch();

    expect(persistence.startBatch).toHaveBeenCalledTimes(1);
    expect(store.getState().results["104.16.1.1:8443"].status).toBe("ok");
    expect(store.getState().results["104.16.1.1:8443"].latencyMs).toBe(80);
    expect(store.getState().results["104.16.2.2:8443"].status).toBe("timeout");
    expect(store.getState().batch).toMatchObject({
      running: false,
      runId: "run-1",
      completed: 2,
      total: 2,
      stopped: false,
    });
  });

  it("进度事件更新已完成与总数", () => {
    const store = createProbeStore(fakePersistence());
    store.getState().applyProgress({ runId: "run-1", completed: 3, total: 8 });

    expect(store.getState().batch).toMatchObject({
      running: true,
      runId: "run-1",
      completed: 3,
      total: 8,
    });
  });

  it("测试进行中重复触发不会再起一轮", async () => {
    let release: (value: unknown) => void = () => {};
    const pending = new Promise((resolve) => {
      release = resolve;
    });
    const persistence = fakePersistence({ startBatch: vi.fn(() => pending) });
    const store = createProbeStore(persistence);

    const first = store.getState().startBatch();
    expect(store.getState().batch.running).toBe(true);
    await store.getState().startBatch();

    expect(persistence.startBatch).toHaveBeenCalledTimes(1);
    release({
      runId: "run-1",
      total: 1,
      completed: 1,
      stopped: false,
      counts: { ok: 1, timeout: 0, fail: 0, untested: 0 },
      rows: [
        { host: "104.16.1.1", port: 8443, alias: "", status: "ok", latencyMs: 80, error: null },
      ],
    });
    await first;
  });

  it("停止按钮只发一次停止信号", async () => {
    const persistence = fakePersistence();
    const store = createProbeStore(persistence);

    await store.getState().cancelBatch();
    await store.getState().cancelBatch();

    expect(persistence.cancelBatch).toHaveBeenCalledTimes(2);
  });

  it("停止后未开始的行落成未测", async () => {
    const store = createProbeStore(
      fakePersistence({
        startBatch: vi.fn(async () => ({
          runId: "run-2",
          total: 3,
          completed: 1,
          stopped: true,
          counts: { ok: 1, timeout: 0, fail: 0, untested: 2 },
          rows: [
            { host: "104.16.1.1", port: 8443, alias: "", status: "ok", latencyMs: 80, error: null },
            { host: "104.16.2.2", port: 8443, alias: "", status: "untested", latencyMs: null, error: null },
            { host: "104.16.3.3", port: 8443, alias: "", status: "untested", latencyMs: null, error: null },
          ],
        })),
      }),
    );

    await store.getState().startBatch();

    expect(store.getState().batch.stopped).toBe(true);
    expect(store.getState().results["104.16.2.2:8443"].status).toBe("untested");
    expect(store.getState().results["104.16.3.3:8443"].status).toBe("untested");
  });

  it("后端报错时结束运行状态并保留原因", async () => {
    const store = createProbeStore(
      fakePersistence({
        startBatch: vi.fn(async () => {
          throw new Error("未找到 sing-box");
        }),
      }),
    );

    await store.getState().startBatch();

    expect(store.getState().batch.running).toBe(false);
    expect(store.getState().error).toContain("sing-box");
  });

  it("坏数据归一成空报告", () => {
    expect(normalizeBatchReport(null)).toEqual({
      runId: "",
      total: 0,
      completed: 0,
      stopped: false,
      counts: { ok: 0, timeout: 0, fail: 0, untested: 0 },
      rows: [],
    });
  });
});
