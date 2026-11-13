/** 导出命令的调用口：生产环境走 Tauri，浏览器原型里直接给可读失败。 */
import { invoke } from "@tauri-apps/api/core";
import type { ExportRow } from "./exportRows";
import { isTauriRuntime } from "./utils";

export type ExportSummary = {
  /** 用户在保存对话框里点了取消。 */
  cancelled: boolean;
  path: string | null;
  rows: number;
  /** 本次导出里缺少归属地的条数。 */
  missingGeo: number;
};

/**
 * 打开系统保存对话框并写出 txt。
 *
 * 范围与顺序由调用方决定：`selectedIds` 非空时导出勾选行，否则导出 `visibleIds`；
 * 两者都按表格当前排序传入。
 */
export async function exportTxt(
  rows: ExportRow[],
  selectedIds: string[],
  visibleIds: string[],
): Promise<ExportSummary> {
  if (!isTauriRuntime()) {
    throw new Error("当前不在 Tauri 窗口内，无法打开保存对话框");
  }
  return invoke<ExportSummary>("export_txt", {
    rows,
    selectedIds,
    visibleIds,
  });
}
