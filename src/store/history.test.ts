import { describe, expect, it } from "vitest";
import {
  buildComparison,
  formatBytes,
  isOverDbLimit,
  normalizeAddressHistory,
  normalizeHistoryResults,
  normalizeTestRuns,
  HISTORY_DB_LIMIT_BYTES,
} from "./history";

describe("历史轮次列表", () => {
  it("按时间倒序归一，坏条目只丢自己", () => {
    const runs = normalizeTestRuns([
      {
        id: "run-old",
        startedAt: "2026-11-12T08:00:00.000Z",
        finishedAt: "2026-11-12T08:01:00.000Z",
        params: { samples: 3, concurrency: 10, kind: "vless" },
        total: 12,
        ok: 9,
        timeout: 2,
        fail: 1,
        addressCount: 12,
      },
      { id: "", startedAt: "2026-11-13T08:00:00.000Z" },
      {
        id: "run-new",
        startedAt: "2026-11-13T08:00:00.000Z",
        finishedAt: null,
        params: "坏数据",
        total: 4,
        ok: -1,
        timeout: "3",
        fail: 2.6,
        addressCount: 4,
      },
    ]);

    expect(runs.map((run) => run.id)).toEqual(["run-new", "run-old"]);
    expect(runs[0].finishedAt).toBeNull();
    expect(runs[0].params).toEqual({});
    // 负数归零，小数取整，字符串计数不采信。
    expect(runs[0].ok).toBe(0);
    expect(runs[0].timeout).toBe(0);
    expect(runs[0].fail).toBe(3);
    expect(runs[1].params).toEqual({ samples: 3, concurrency: 10, kind: "vless" });
  });

  it("不是数组时返回空列表", () => {
    expect(normalizeTestRuns(null)).toEqual([]);
    expect(normalizeTestRuns({ runs: [] })).toEqual([]);
  });
});

describe("数据库体积", () => {
  it("按 KB / MB / GB 格式化", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(13_002_342)).toBe("12.4 MB");
    expect(formatBytes(2 * 1024 ** 3)).toBe("2.0 GB");
    expect(formatBytes(-10)).toBe("0 B");
  });

  it("超过 50MB 才提示清理", () => {
    expect(isOverDbLimit(HISTORY_DB_LIMIT_BYTES)).toBe(true);
    expect(isOverDbLimit(HISTORY_DB_LIMIT_BYTES - 1)).toBe(false);
  });
});

describe("历史结果", () => {
  it("字段与主表格一致，坏数据不伪装成成功", () => {
    const rows = normalizeHistoryResults([
      {
        host: "104.16.1.1",
        port: 8443,
        alias: "洛杉矶",
        status: "ok",
        latencyMs: 88.4,
        speedMbps: 12.34,
        speedStatus: "ok",
        country: "US",
        zh: "美国",
        en: "United States",
      },
      {
        host: "104.16.1.2",
        port: 8443,
        status: "ok",
        latencyMs: null,
        speedStatus: "ok",
        speedMbps: 0,
      },
      { host: "", port: 0, status: "ok", latencyMs: 10 },
      {
        host: "104.16.1.3",
        port: 2053,
        status: "timeout",
        error: "10s 内未收到响应",
      },
    ]);

    expect(rows).toHaveLength(3);
    expect(rows[0]).toMatchObject({
      host: "104.16.1.1",
      port: 8443,
      status: "ok",
      latencyMs: 88,
      speedMbps: 12.34,
      speedStatus: "ok",
      zh: "美国",
    });
    // 成功却没有延迟、速度却为 0 的行按失败处理，不显示假的 0。
    expect(rows[1].status).toBe("fail");
    expect(rows[1].latencyMs).toBeNull();
    expect(rows[1].speedStatus).toBe("fail");
    expect(rows[1].speedMbps).toBeNull();
    expect(rows[2].status).toBe("timeout");
    expect(rows[2].latencyMs).toBeNull();
    expect(rows[2].error).toBe("10s 内未收到响应");
  });
});

describe("同一地址跨轮对比", () => {
  it("按时间升序排列，坏点与空 runId 被丢弃", () => {
    const points = normalizeAddressHistory({
      host: "104.16.1.1",
      port: 8443,
      points: [
        { runId: "run-b", startedAt: "2026-11-13T08:00:00.000Z", status: "ok", latencyMs: 70 },
        { runId: "", startedAt: "2026-11-13T07:00:00.000Z", status: "ok", latencyMs: 60 },
        { runId: "run-a", startedAt: "2026-11-12T08:00:00.000Z", status: "ok", latencyMs: 100, speedMbps: 8.2, speedStatus: "ok" },
      ],
    });

    expect(points.map((point) => point.runId)).toEqual(["run-a", "run-b"]);
    expect(points[0].latencyMs).toBe(100);
    expect(points[0].speedMbps).toBe(8.2);
  });

  it("对比条按各自最大值换算成百分比", () => {
    const points = normalizeAddressHistory({
      points: [
        { runId: "a", startedAt: "2026-11-12T08:00:00.000Z", status: "ok", latencyMs: 100, speedMbps: 10, speedStatus: "ok" },
        { runId: "b", startedAt: "2026-11-13T08:00:00.000Z", status: "ok", latencyMs: 50, speedMbps: 5, speedStatus: "ok" },
        { runId: "c", startedAt: "2026-11-14T08:00:00.000Z", status: "timeout", latencyMs: null, speedStatus: "untested" },
      ],
    });
    const rows = buildComparison(points);

    expect(rows.map((row) => row.latencyPct)).toEqual([100, 50, 0]);
    expect(rows.map((row) => row.speedPct)).toEqual([100, 50, 0]);
    expect(rows[2].latencyMs).toBeNull();
    expect(rows[2].speedMbps).toBeNull();
  });

  it("没有数据时百分比为 0，不产生 NaN", () => {
    const rows = buildComparison([]);
    expect(rows).toEqual([]);
    expect(buildComparison([
      { runId: "a", startedAt: "2026-11-12T08:00:00.000Z", status: "fail", latencyMs: null, speedMbps: null, speedStatus: "untested" },
    ])[0].latencyPct).toBe(0);
  });
});
