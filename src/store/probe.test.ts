import { describe, expect, it, vi } from "vitest";
import {
  createProbeStore,
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
