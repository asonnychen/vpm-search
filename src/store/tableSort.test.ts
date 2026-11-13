import { describe, expect, it } from "vitest";
import { sortRows } from "./tableSort";

type Row = { id: string; latency: number | null; speed: number | null };

function rows(): Row[] {
  return [
    { id: "untested", latency: null, speed: null },
    { id: "slow", latency: 300, speed: 1.2 },
    { id: "timeout", latency: null, speed: null },
    { id: "fast", latency: 80, speed: 9.5 },
    { id: "fail", latency: null, speed: null },
    { id: "mid", latency: 150, speed: 4.0 },
  ];
}

describe("表格排序", () => {
  it("升序时空值永远在末尾", () => {
    const sorted = sortRows(rows(), "latency", "asc").map((row) => row.id);
    expect(sorted.slice(0, 3)).toEqual(["fast", "mid", "slow"]);
    expect(sorted.slice(3).sort()).toEqual(["fail", "timeout", "untested"]);
  });

  it("降序时空值仍在末尾", () => {
    const sorted = sortRows(rows(), "latency", "desc").map((row) => row.id);
    expect(sorted.slice(0, 3)).toEqual(["slow", "mid", "fast"]);
    expect(sorted.slice(3).sort()).toEqual(["fail", "timeout", "untested"]);
  });

  it("超时与失败不参与数值翻转", () => {
    const sorted = sortRows(rows(), "speed", "desc").map((row) => row.id);
    expect(sorted.slice(0, 3)).toEqual(["fast", "mid", "slow"]);
    expect(sorted.slice(3).sort()).toEqual(["fail", "timeout", "untested"]);
  });
});
