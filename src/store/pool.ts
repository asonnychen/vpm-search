import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { errorMessage, isRecord, isTauriRuntime } from "./utils";

/** 一条优选源。 */
export type PoolSource = {
  url: string;
  /** 来源标识，形如 `BestCF·vps789/top10.txt`。 */
  name: string;
  checked: boolean;
  /** 页面标注「更新异常」。 */
  broken: boolean;
  entryCount: number;
  fetchedAt: string | null;
  lastError: string | null;
};

/** 池里的一条优选地址。 */
export type PoolEntry = {
  host: string;
  port: number;
  alias: string;
  /** 来源标识。 */
  source: string;
  addedAt: string;
};

export type PoolSnapshot = {
  sources: PoolSource[];
  entries: PoolEntry[];
  /** 最近一次抓取时间（ISO 8601）。 */
  fetchedAt: string | null;
  /** 本次抓取被去重掉的条数。 */
  duplicates: number;
};

export const EMPTY_SNAPSHOT: PoolSnapshot = {
  sources: [],
  entries: [],
  fetchedAt: null,
  duplicates: 0,
};

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function nullableStr(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function num(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/** 把后端返回的任意 JSON 归一成快照，坏字段只丢自己。 */
export function normalizeSnapshot(raw: unknown): PoolSnapshot {
  if (!isRecord(raw)) return EMPTY_SNAPSHOT;

  const sources: PoolSource[] = Array.isArray(raw.sources)
    ? raw.sources.filter(isRecord).map((item) => ({
        url: str(item.url),
        name: str(item.name),
        checked: bool(item.checked, false),
        broken: bool(item.broken, false),
        entryCount: Math.max(0, Math.round(num(item.entryCount))),
        fetchedAt: nullableStr(item.fetchedAt),
        lastError: nullableStr(item.lastError),
      }))
    : [];

  const entries: PoolEntry[] = Array.isArray(raw.entries)
    ? raw.entries.filter(isRecord).map((item) => ({
        host: str(item.host),
        port: Math.round(num(item.port)),
        alias: str(item.alias),
        source: str(item.source),
        addedAt: str(item.addedAt),
      }))
    : [];

  return {
    sources: sources.filter((s) => s.url !== ""),
    entries: entries.filter((e) => e.host !== "" && e.port > 0),
    fetchedAt: nullableStr(raw.fetchedAt),
    duplicates: Math.max(0, Math.round(num(raw.duplicates))),
  };
}

/** 抓取时间显示成 `11-13 12:03`，同一天时省掉日期。 */
export function formatFetchedAt(iso: string | null, now = new Date()): string {
  if (!iso) return "尚未抓取";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  const clock = `${pad(at.getHours())}:${pad(at.getMinutes())}`;
  const sameDay =
    at.getFullYear() === now.getFullYear() &&
    at.getMonth() === now.getMonth() &&
    at.getDate() === now.getDate();
  return sameDay ? clock : `${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${clock}`;
}

/** 持久化端口：默认接 Tauri 命令，测试里换成假实现。 */
export type PoolPersistence = {
  load: () => Promise<unknown>;
  refreshCatalog: () => Promise<unknown>;
  fetchPool: (
    sourceUrls: string[],
    extraSourceUrls: string[],
    manualAddresses: string,
  ) => Promise<unknown>;
  setChecked: (url: string, checked: boolean) => Promise<void>;
};

export type PoolStore = PoolSnapshot & {
  loaded: boolean;
  /** 正在抓取源。 */
  fetching: boolean;
  /** 正在刷新源清单。 */
  refreshing: boolean;
  error: string | null;
  load: () => Promise<void>;
  refreshCatalog: () => Promise<void>;
  fetchPool: (manualAddresses: string, extraSourceUrls?: string) => Promise<void>;
  toggleSource: (url: string, checked: boolean) => Promise<void>;
};

export function createPoolStore(persistence: PoolPersistence) {
  return create<PoolStore>()((set, get) => {
    const apply = (snapshot: PoolSnapshot) => {
      set({
        sources: snapshot.sources,
        entries: snapshot.entries,
        fetchedAt: snapshot.fetchedAt,
        duplicates: snapshot.duplicates,
      });
    };

    return {
      ...EMPTY_SNAPSHOT,
      loaded: false,
      fetching: false,
      refreshing: false,
      error: null,

      load: async () => {
        try {
          apply(normalizeSnapshot(await persistence.load()));
          set({ loaded: true, error: null });
        } catch (error) {
          // 读失败保留空池，界面照常可用。
          set({ loaded: true, error: errorMessage(error) });
        }
      },

      refreshCatalog: async () => {
        set({ refreshing: true, error: null });
        try {
          apply(normalizeSnapshot(await persistence.refreshCatalog()));
          set({ refreshing: false });
        } catch (error) {
          set({ refreshing: false, error: errorMessage(error) });
        }
      },

      fetchPool: async (manualAddresses, extraSourceUrls = "") => {
        const urls = get()
          .sources.filter((s) => s.checked)
          .map((s) => s.url);
        // 左侧粘贴的 txt 源地址每行一个，同样参与本次抓取。
        const pasted = Array.from(
          new Set(
            extraSourceUrls
              .split(/\r?\n/)
              .map((line) => line.trim())
              .filter((line) => line !== ""),
          ),
        );
        if (urls.length === 0 && pasted.length === 0 && manualAddresses.trim() === "") {
          set({ error: "没有勾选任何优选源，也没有手动地址" });
          return;
        }

        set({ fetching: true, error: null });
        try {
          apply(
            normalizeSnapshot(
              await persistence.fetchPool(urls, pasted, manualAddresses),
            ),
          );
          set({ fetching: false });
        } catch (error) {
          // 抓取失败不清空上一次的结果，只把原因写出来。
          set({ fetching: false, error: errorMessage(error) });
        }
      },

      toggleSource: async (url, checked) => {
        set({
          sources: get().sources.map((s) => (s.url === url ? { ...s, checked } : s)),
        });
        try {
          await persistence.setChecked(url, checked);
        } catch (error) {
          set({ error: errorMessage(error) });
        }
      },
    };
  });
}

/** 生产环境走 Tauri 命令；浏览器里跑原型时返回空池。 */
export const tauriPoolPersistence: PoolPersistence = {
  load: () => (isTauriRuntime() ? invoke<unknown>("get_pool") : Promise.resolve(null)),
  refreshCatalog: () =>
    isTauriRuntime() ? invoke<unknown>("refresh_source_catalog") : Promise.resolve(null),
  fetchPool: (sourceUrls, extraSourceUrls, manualAddresses) =>
    isTauriRuntime()
      ? invoke<unknown>("fetch_pool", {
          sourceUrls,
          extraSourceUrls,
          manualAddresses,
        })
      : Promise.resolve(null),
  setChecked: (url, checked) =>
    isTauriRuntime()
      ? invoke<void>("set_source_checked", { url, checked }).then(() => undefined)
      : Promise.resolve(),
};

export const usePoolStore = createPoolStore(tauriPoolPersistence);

export const selectPoolSources = (state: PoolStore) => state.sources;
export const selectPoolEntries = (state: PoolStore) => state.entries;
