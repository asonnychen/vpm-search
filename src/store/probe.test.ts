import { describe, expect, it, vi } from "vitest";
import {
  PROBE_ROW_FLUSH_MS,
  createProbeRowQueue,
  createProbeStore,
  normalizeBatchReport,
  normalizeGeoReport,
  normalizeProbeResult,
  normalizeProbeRow,
  normalizeSpeedReport,
  probeKey,
  type ProbePersistence,
  type ProbeRowEvent,
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
    speedBatch: vi.fn(async () => ({
      runId: "run-1",
      total: 2,
      completed: 2,
      stopped: false,
      counts: { ok: 1, fail: 1, untested: 0 },
      rows: [
        { host: "104.16.1.1", port: 8443, alias: "", status: "ok", mbps: 12.3, error: null },
        { host: "104.16.2.2", port: 8443, alias: "", status: "fail", mbps: null, error: "连接被拒绝" },
      ],
    })),
    cancelBatch: vi.fn(async () => true),
    geoBatch: vi.fn(async () => ({
      runId: "run-1",
      total: 2,
      completed: 2,
      stopped: false,
      counts: { ok: 2, fail: 0, untested: 0 },
      rows: [
        {
          host: "104.16.1.1",
          port: 8443,
          alias: "",
          status: "ok",
          ip: "104.16.1.1",
          country: "US",
          zh: "美国",
          en: "United States",
          asn: 13335,
          error: null,
          source: "offline",
        },
        {
          host: "104.16.2.2",
          port: 8443,
          alias: "",
          status: "ok",
          ip: "198.51.100.7",
          country: "CA",
          zh: "加拿大",
          en: "Canada",
          asn: 15169,
          error: null,
          source: "online",
        },
      ],
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
        error: "连接失败：连接被拒绝",
      })),
    });
    const store = createProbeStore(persistence);

    await store.getState().testOne("104.16.1.1", 8443);

    const result = store.getState().results["104.16.1.1:8443"];
    expect(result.status).toBe("fail");
    expect(result.latencyMs).toBeNull();
    expect(result.error).toContain("连接被拒绝");
    // 失败原因要留在页面顶部的提示里。
    expect(store.getState().error).toContain("连接被拒绝");
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
          throw new Error("池里还没有地址，请先抓取优选源");
        }),
      }),
    );

    await store.getState().startBatch();

    expect(store.getState().batch.running).toBe(false);
    expect(store.getState().error).toContain("池里还没有地址");
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

describe("测速", () => {
  it("测速结果写进速度表且不清空已有延迟结果", async () => {
    const persistence = fakePersistence();
    const store = createProbeStore(persistence);

    await store.getState().testOne("104.16.1.1", 8443);
    await store.getState().startSpeedBatch([{ host: "104.16.1.1", port: 8443 }]);

    expect(persistence.speedBatch).toHaveBeenCalledWith([
      { host: "104.16.1.1", port: 8443 },
    ]);
    // 延迟结果仍在，测速单独落进另一张表。
    expect(store.getState().results["104.16.1.1:8443"].latencyMs).toBe(120);
    expect(store.getState().speeds["104.16.1.1:8443"]).toEqual({
      status: "ok",
      mbps: 12.3,
      error: null,
    });
    expect(store.getState().speeds["104.16.2.2:8443"]).toEqual({
      status: "fail",
      mbps: null,
      error: "连接被拒绝",
    });
  });

  it("成功却没有正速度时按失败处理，不显示 0", () => {
    const report = normalizeSpeedReport({
      runId: "run-1",
      rows: [
        { host: "104.16.1.1", port: 8443, status: "ok", mbps: 0 },
        { host: "104.16.1.2", port: 8443, status: "ok", mbps: -1 },
      ],
    });

    expect(report.rows[0]).toMatchObject({ status: "fail", mbps: null });
    expect(report.rows[1]).toMatchObject({ status: "fail", mbps: null });
  });

  it("停止后未测的行不覆盖已有速度", async () => {
    const first = fakePersistence();
    const store = createProbeStore(first);
    await store.getState().startSpeedBatch();

    const second = fakePersistence({
      speedBatch: vi.fn(async () => ({
        runId: "run-2",
        total: 1,
        completed: 0,
        stopped: true,
        counts: { ok: 0, fail: 0, untested: 1 },
        rows: [
          { host: "104.16.1.1", port: 8443, alias: "", status: "untested", mbps: null, error: null },
        ],
      })),
    });
    const restarted = createProbeStore(second);
    restarted.setState({ speeds: store.getState().speeds });
    await restarted.getState().startSpeedBatch();

    expect(restarted.getState().speeds["104.16.1.1:8443"].mbps).toBe(12.3);
  });

  it("测速中标记为 speed 模式并记录进度", () => {
    const store = createProbeStore(fakePersistence());
    store.getState().applyProgress({ runId: "run-1", mode: "speed", completed: 2, total: 5 });

    expect(store.getState().batch).toMatchObject({
      running: true,
      mode: "speed",
      completed: 2,
      total: 5,
    });
  });
});

describe("归属地", () => {
  it("归属地写进归属地表且不清空延迟与速度", async () => {
    const persistence = fakePersistence();
    const store = createProbeStore(persistence);

    await store.getState().testOne("104.16.1.1", 8443);
    await store.getState().startSpeedBatch([{ host: "104.16.1.1", port: 8443 }]);
    await store.getState().startGeoBatch([{ host: "104.16.1.1", port: 8443 }]);

    expect(persistence.geoBatch).toHaveBeenCalledWith([
      { host: "104.16.1.1", port: 8443 },
    ]);
    // 延迟与速度都不受归属地影响。
    expect(store.getState().results["104.16.1.1:8443"].latencyMs).toBe(120);
    expect(store.getState().speeds["104.16.1.1:8443"].mbps).toBe(12.3);

    // 归属地只代表地址本身，带 IP 与中英文名。
    expect(store.getState().geos["104.16.1.1:8443"]).toMatchObject({
      ip: "104.16.1.1",
      zh: "美国",
      en: "United States",
      asn: 13335,
    });
    expect(store.getState().geos["104.16.2.2:8443"]).toMatchObject({
      zh: "加拿大",
      en: "Canada",
    });
  });

  it("成功却没有地名时按失败处理，不写空字符串", () => {
    const report = normalizeGeoReport({
      runId: "run-1",
      rows: [
        { host: "104.16.1.1", port: 8443, status: "ok", zh: "", en: "" },
        { host: "104.16.1.2", port: 8443, status: "fail", error: "归属地查询失败" },
      ],
    });

    expect(report.rows[0]).toMatchObject({ status: "fail", zh: "" });
    expect(report.rows[1]).toMatchObject({
      status: "fail",
      error: "归属地查询失败",
    });
  });

  it("查询失败或未测的行不覆盖已有归属地", async () => {
    const first = fakePersistence();
    const store = createProbeStore(first);
    await store.getState().startGeoBatch();

    const second = fakePersistence({
      geoBatch: vi.fn(async () => ({
        runId: "run-2",
        total: 1,
        completed: 0,
        stopped: true,
        counts: { ok: 0, fail: 1, untested: 0 },
        rows: [
          {
            host: "104.16.1.1",
            port: 8443,
            alias: "",
            status: "fail",
            ip: null,
            country: null,
            zh: "",
            en: "",
            asn: null,
            error: "解析失败",
            source: "",
          },
        ],
      })),
    });
    const restarted = createProbeStore(second);
    restarted.setState({ geos: store.getState().geos });
    await restarted.getState().startGeoBatch();

    expect(restarted.getState().geos["104.16.1.1:8443"].zh).toBe("美国");
  });

  it("归属地查询中标记为 geo 模式并记录进度", () => {
    const store = createProbeStore(fakePersistence());
    store.getState().applyProgress({ runId: "run-1", mode: "geo", completed: 1, total: 4 });

    expect(store.getState().batch).toMatchObject({
      running: true,
      mode: "geo",
      completed: 1,
      total: 4,
    });
  });
});

/** 造一条实时结果事件，未指定的字段一律为空。 */
function rowEvent(overrides: Partial<ProbeRowEvent> = {}): ProbeRowEvent {
  return {
    runId: "run-1",
    kind: "delay",
    host: "104.16.1.1",
    port: 8443,
    status: "ok",
    latencyMs: null,
    speedMbps: null,
    countryZh: null,
    countryEn: null,
    colo: null,
    error: null,
    ...overrides,
  };
}

describe("实时结果事件", () => {
  it("归一化事件，缺地址或坏数据直接丢弃", () => {
    expect(normalizeProbeRow(null)).toBeNull();
    expect(normalizeProbeRow({ host: "", port: 443 })).toBeNull();
    expect(normalizeProbeRow({ host: "104.16.1.1", port: 0 })).toBeNull();

    expect(
      normalizeProbeRow({
        runId: "run-1",
        kind: "delay",
        host: "104.16.1.1",
        port: 8443,
        status: "ok",
        latencyMs: 88.4,
        error: null,
      }),
    ).toMatchObject({ kind: "delay", latencyMs: 88, speedMbps: null });
  });

  it("延迟、速度、归属地各自写进对应的表", () => {
    const store = createProbeStore(fakePersistence());

    store.getState().applyRows([
      rowEvent({ latencyMs: 88 }),
      rowEvent({ kind: "speed", status: "ok", speedMbps: 9.5 }),
      rowEvent({ kind: "geo", countryZh: "加拿大", countryEn: "Canada" }),
    ]);

    expect(store.getState().results["104.16.1.1:8443"]).toMatchObject({
      status: "ok",
      latencyMs: 88,
    });
    expect(store.getState().speeds["104.16.1.1:8443"]).toMatchObject({
      status: "ok",
      mbps: 9.5,
    });
    expect(store.getState().geos["104.16.1.1:8443"]).toMatchObject({
      zh: "加拿大",
      en: "Canada",
    });
  });

  it("失败不抹掉已有归属地，未测的测速不覆盖旧速度", () => {
    const store = createProbeStore(fakePersistence());
    store.setState({
      geos: {
        "104.16.1.1:8443": {
          ip: "104.16.1.1",
          country: "US",
          zh: "美国",
          en: "United States",
          asn: null,
          source: "offline",
        },
      },
      speeds: { "104.16.1.1:8443": { status: "ok", mbps: 12.3, error: null } },
    });

    store.getState().applyRows([
      rowEvent({ kind: "geo", status: "fail", error: "解析失败" }),
      rowEvent({ kind: "speed", status: "untested" }),
    ]);

    expect(store.getState().geos["104.16.1.1:8443"].zh).toBe("美国");
    expect(store.getState().speeds["104.16.1.1:8443"].mbps).toBe(12.3);
  });

  it("整轮收尾后迟到的事件被丢弃，最终结果以报告为准", async () => {
    const store = createProbeStore(fakePersistence());
    await store.getState().startBatch();

    store.getState().applyRows([
      rowEvent({ status: "fail", latencyMs: null, error: "迟到的失败" }),
    ]);

    expect(store.getState().results["104.16.1.1:8443"]).toMatchObject({
      status: "ok",
      latencyMs: 80,
    });
  });

  it("同一窗口内的同类事件合并成一次写入", () => {
    vi.useFakeTimers();
    try {
      const batches: ProbeRowEvent[][] = [];
      const queue = createProbeRowQueue((rows) => batches.push(rows), 50);

      queue.push(rowEvent({ latencyMs: 100 }));
      queue.push(rowEvent({ latencyMs: 120 }));
      queue.push(rowEvent({ host: "104.16.2.2", latencyMs: 60 }));

      // 还没到窗口，先不写。
      expect(batches).toHaveLength(0);
      vi.advanceTimersByTime(50);

      expect(batches).toHaveLength(1);
      // 同一行只保留最后一次，不同行各自保留。
      expect(batches[0]).toHaveLength(2);
      expect(batches[0].find((row) => row.host === "104.16.1.1")?.latencyMs).toBe(120);
    } finally {
      vi.useRealTimers();
    }
  });

  it("默认合并窗口是 180ms，同一批内同一行只留最后一次", () => {
    vi.useFakeTimers();
    try {
      const batches: ProbeRowEvent[][] = [];
      const queue = createProbeRowQueue((rows) => batches.push(rows));

      queue.push(rowEvent({ latencyMs: 100 }));
      queue.push(rowEvent({ latencyMs: 90 }));
      queue.push(rowEvent({ latencyMs: 80 }));

      // 窗口没到，一条都不写。
      vi.advanceTimersByTime(PROBE_ROW_FLUSH_MS - 1);
      expect(batches).toHaveLength(0);

      vi.advanceTimersByTime(1);
      expect(batches).toHaveLength(1);
      expect(batches[0]).toHaveLength(1);
      expect(batches[0][0].latencyMs).toBe(80);
    } finally {
      vi.useRealTimers();
    }
  });

  it("整轮报告立即生效，节流中的中间态不会拖后覆盖最终结果", async () => {
    const store = createProbeStore(fakePersistence());
    const queue = createProbeRowQueue((rows) => store.getState().applyRows(rows));

    // 节流窗口里先攒一条中间态。
    queue.push(rowEvent({ latencyMs: 999 }));
    await store.getState().startBatch();

    // 报告已经落地，立刻就是最终值。
    expect(store.getState().results["104.16.1.1:8443"].latencyMs).toBe(80);

    // 迟到的合并批被丢弃，不会把最终结果改回中间态。
    queue.flush();
    expect(store.getState().results["104.16.1.1:8443"].latencyMs).toBe(80);
  });

  it("取消订阅时丢弃未写出的结果", () => {
    vi.useFakeTimers();
    try {
      const batches: ProbeRowEvent[][] = [];
      const queue = createProbeRowQueue((rows) => batches.push(rows), 50);

      queue.push(rowEvent({ latencyMs: 100 }));
      queue.clear();
      vi.advanceTimersByTime(100);

      expect(batches).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
