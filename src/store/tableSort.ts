/** 表格排序：延迟 / 速度的数值排序，空值（超时、失败、未测）永远置底。 */

export type SortKey = "latency" | "speed";
export type SortDir = "asc" | "desc";

export type SortableRow = {
  latency: number | null;
  speed: number | null;
};

/**
 * 按指定列排序，不改原数组。
 *
 * 升序与降序都只翻转有值的行；空值不参与翻转，始终排在末尾，
 * 这样超时与失败不会因为切换方向跑到最前面。
 */
export function sortRows<T extends SortableRow>(
  rows: T[],
  key: SortKey,
  dir: SortDir,
): T[] {
  return [...rows].sort((a, b) => {
    const av = a[key];
    const bv = b[key];
    if (av === null && bv === null) return 0;
    if (av === null) return 1;
    if (bv === null) return -1;
    return dir === "asc" ? av - bv : bv - av;
  });
}
