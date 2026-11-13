/** 跨 store 与组件共用的运行时判断与错误归一工具。 */

/** 是否运行在 Tauri 窗口内；浏览器原型里为 false。 */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 把任意异常归一成可展示的一行文字。 */
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 是否为普通对象（排除 null 与数组）。 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
