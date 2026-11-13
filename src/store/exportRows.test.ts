import { describe, expect, it } from "vitest";
import {
  countMissingGeo,
  pickExportRange,
  type ExportRow,
} from "./exportRows";

function row(
  host: string,
  port: number,
  geo: ExportRow["geo"],
  source = "BestCF·all.txt",
): ExportRow {
  return { id: `${host}:${port}`, host, port, alias: "", source, geo };
}

const canada = { zh: "加拿大", en: "Canada", colo: null };
const japan = { zh: "日本", en: "Japan", colo: "NRT" };

describe("导出范围选择", () => {
  it("有勾选行时只取勾选行", () => {
    const rows = [
      row("a.example", 443, canada),
      row("b.example", 443, japan),
      row("c.example", 443, canada),
    ];
    const chosen = pickExportRange(rows, ["c.example:443", "a.example:443"], [
      "b.example:443",
      "a.example:443",
      "c.example:443",
    ]);
    expect(chosen.map((item) => item.id)).toEqual([
      "a.example:443",
      "c.example:443",
    ]);
  });

  it("没有勾选行时取当前筛选后的可见行", () => {
    const rows = [
      row("a.example", 443, canada),
      row("b.example", 443, japan),
      row("c.example", 443, canada),
    ];
    const chosen = pickExportRange(rows, [], [
      "c.example:443",
      "a.example:443",
    ]);
    expect(chosen.map((item) => item.id)).toEqual([
      "a.example:443",
      "c.example:443",
    ]);
  });

  it("顺序跟表格当前排序，而不是勾选先后", () => {
    const rows = [
      row("slow.example", 443, canada),
      row("fast.example", 443, canada),
      row("mid.example", 443, canada),
    ];
    const chosen = pickExportRange(
      rows,
      ["mid.example:443", "fast.example:443"],
      ["slow.example:443"],
    );
    expect(chosen.map((item) => item.host)).toEqual([
      "fast.example",
      "mid.example",
    ]);
  });

  it("空勾选且空可见时没有可导出的行", () => {
    const rows = [row("a.example", 443, canada)];
    expect(pickExportRange(rows, [], [])).toEqual([]);
    // 勾选里的未知地址被忽略。
    expect(pickExportRange(rows, ["不存在:1"], [])).toEqual([]);
  });
});

describe("归属地缺失统计", () => {
  it("中英文名都为空才算缺失，机房单独存在不算", () => {
    const rows = [
      row("a.example", 443, canada),
      row("b.example", 443, null),
      row("c.example", 443, { zh: "", en: "", colo: "LAX" }),
      row("d.example", 443, { zh: "", en: "", colo: null }),
    ];
    expect(countMissingGeo(rows)).toBe(3);
    expect(countMissingGeo([])).toBe(0);
  });
});
