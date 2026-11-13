import { describe, expect, it, vi } from "vitest";
import {
  EMPTY_SNAPSHOT,
  createPoolStore,
  formatFetchedAt,
  normalizeSnapshot,
  type PoolPersistence,
} from "./pool";

const SNAPSHOT = {
  sources: [
    {
      url: "https://bestcf.pages.dev/domain/all.txt",
      name: "BestCF·domain/all.txt",
      checked: true,
      broken: false,
      entryCount: 2,
      fetchedAt: "2026-11-13T04:05:06.000Z",
      lastError: null,
    },
    {
      url: "https://bestcf.pages.dev/luoli/all.txt",
      name: "BestCF·luoli/all.txt",
      checked: false,
      broken: true,
      entryCount: 0,
      fetchedAt: null,
      lastError: "HTTP 404",
    },
  ],
  entries: [
    {
      host: "1.1.1.1",
      port: 443,
      alias: "一 | 备注",
      source: "BestCF·domain/all.txt",
      addedAt: "2026-11-13T04:05:06.000Z",
    },
  ],
  fetchedAt: "2026-11-13T04:05:06.000Z",
  duplicates: 1,
};

function fakePersistence(overrides: Partial<PoolPersistence> = {}): PoolPersistence {
  return {
    load: vi.fn(async () => SNAPSHOT),
    refreshCatalog: vi.fn(async () => SNAPSHOT),
    fetchPool: vi.fn(async () => SNAPSHOT),
    setChecked: vi.fn(async () => {}),
    ...overrides,
  };
}

describe("normalizeSnapshot", () => {
  it("保留字段并丢弃空地址", () => {
    const snapshot = normalizeSnapshot({
      sources: [{ url: "", name: "坏源" }, { url: "https://a/all.txt", name: "源" }],
      entries: [{ host: "", port: 443 }, { host: "1.1.1.1", port: 443, alias: "备注" }],
      fetchedAt: "2026-11-13T04:05:06.000Z",
    });

    expect(snapshot.sources).toHaveLength(1);
    expect(snapshot.entries).toHaveLength(1);
    expect(snapshot.entries[0]!.alias).toBe("备注");
  });

  it("完全无法识别的输入返回空池", () => {
    expect(normalizeSnapshot(null)).toEqual(EMPTY_SNAPSHOT);
    expect(normalizeSnapshot("乱码")).toEqual(EMPTY_SNAPSHOT);
  });
});

describe("抓取时间显示", () => {
  it("当天只显示时分，跨天带日期", () => {
    const now = new Date(2026, 10, 13, 20, 0);
    const sameDay = new Date(2026, 10, 13, 12, 3).toISOString();
    const otherDay = new Date(2026, 10, 12, 12, 3).toISOString();

    expect(formatFetchedAt(sameDay, now)).toMatch(/^\d{2}:\d{2}$/);
    expect(formatFetchedAt(otherDay, now)).toMatch(/^\d{2}-\d{2} \d{2}:\d{2}$/);
    expect(formatFetchedAt(null, now)).toBe("尚未抓取");
  });
});

describe("池 store", () => {
  it("启动时读取上次的池与抓取时间", async () => {
    const store = createPoolStore(fakePersistence());
    await store.getState().load();

    const state = store.getState();
    expect(state.loaded).toBe(true);
    expect(state.sources).toHaveLength(2);
    expect(state.entries[0]!.host).toBe("1.1.1.1");
    expect(state.fetchedAt).toBe("2026-11-13T04:05:06.000Z");
    expect(state.duplicates).toBe(1);
  });

  it("抓取只带勾选的源，并把手动地址与粘贴的源一起提交", async () => {
    const fetchPool = vi.fn(async () => SNAPSHOT);
    const store = createPoolStore(fakePersistence({ fetchPool }));
    await store.getState().load();

    await store.getState().fetchPool(
      "3.3.3.3:443#手动",
      "https://example.com/all.txt\n\n https://example.com/all.txt \nhttps://b.example.com/x.txt",
    );

    expect(fetchPool).toHaveBeenCalledWith(
      ["https://bestcf.pages.dev/domain/all.txt"],
      ["https://example.com/all.txt", "https://b.example.com/x.txt"],
      "3.3.3.3:443#手动",
    );
    expect(store.getState().entries).toHaveLength(1);
  });

  it("没有勾选也没有手动地址时直接给出提示", async () => {
    const fetchPool = vi.fn(async () => SNAPSHOT);
    const store = createPoolStore(
      fakePersistence({
        load: async () => ({ sources: [], entries: [], fetchedAt: null }),
        fetchPool,
      }),
    );
    await store.getState().load();

    await store.getState().fetchPool("   ");

    expect(fetchPool).not.toHaveBeenCalled();
    expect(store.getState().error).toContain("没有勾选");
  });

  it("抓取失败时保留上一次结果并显示可读原因", async () => {
    const store = createPoolStore(
      fakePersistence({
        fetchPool: async () => {
          throw new Error("抓取优选源清单失败：请求超时（10s）");
        },
      }),
    );
    await store.getState().load();
    expect(store.getState().entries).toHaveLength(1);

    await store.getState().fetchPool("");

    const state = store.getState();
    expect(state.entries).toHaveLength(1);
    expect(state.fetching).toBe(false);
    expect(state.error).toContain("请求超时");
  });

  it("勾选源立即生效并落库", async () => {
    const setChecked = vi.fn(async () => {});
    const store = createPoolStore(fakePersistence({ setChecked }));
    await store.getState().load();

    await store.getState().toggleSource("https://bestcf.pages.dev/luoli/all.txt", true);

    expect(store.getState().sources[1]!.checked).toBe(true);
    expect(setChecked).toHaveBeenCalledWith(
      "https://bestcf.pages.dev/luoli/all.txt",
      true,
    );
  });
});
