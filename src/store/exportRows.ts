/** 导出 txt 的行数据与范围选择（阶段 10）。 */

/** 一条待导出的地址；`geo` 缺失时导出内容里该段留空。 */
export type ExportRow = {
  /** 表格行的键，`host:port`。 */
  id: string;
  host: string;
  port: number;
  alias: string;
  /** 来源标识，进入导出内容。 */
  source: string;
  geo: { zh: string; en: string } | null;
};

/**
 * 选导出范围：有勾选行就用勾选行，否则用可见行。
 *
 * 结果顺序跟随 `rows`（表格当前排序），而不是勾选或点击的先后；
 * 未知 id 直接忽略，不凭空造行。
 */
export function pickExportRange(
  rows: ExportRow[],
  selectedIds: string[],
  visibleIds: string[],
): ExportRow[] {
  const wanted = new Set(selectedIds.length > 0 ? selectedIds : visibleIds);
  if (wanted.size === 0) return [];
  return rows.filter((row) => wanted.has(row.id));
}

/** 范围内归属地缺失（中英文名都为空）的行数。 */
export function countMissingGeo(rows: ExportRow[]): number {
  return rows.filter((row) => !row.geo?.zh && !row.geo?.en).length;
}