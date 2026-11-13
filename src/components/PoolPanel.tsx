import { formatFetchedAt, usePoolStore } from "../store/pool";
import { selectLastInput, useSettingsStore } from "../store/settings";
import { Btn, cn } from "./ui";

export default function PoolPanel() {
  const lastInput = useSettingsStore(selectLastInput);
  const open = useSettingsStore((state) => state.settings.ui.poolOpen);
  const patch = useSettingsStore((state) => state.patch);
  const setOpen = (next: boolean) => patch({ ui: { poolOpen: next } });

  const sources = usePoolStore((state) => state.sources);
  const entries = usePoolStore((state) => state.entries);
  const fetchedAt = usePoolStore((state) => state.fetchedAt);
  const duplicates = usePoolStore((state) => state.duplicates);
  const fetching = usePoolStore((state) => state.fetching);
  const refreshing = usePoolStore((state) => state.refreshing);
  const error = usePoolStore((state) => state.error);
  const refreshCatalog = usePoolStore((state) => state.refreshCatalog);
  const fetchPool = usePoolStore((state) => state.fetchPool);
  const toggleSource = usePoolStore((state) => state.toggleSource);

  const checkedCount = sources.filter((s) => s.checked).length;
  const failedCount = sources.filter((s) => s.lastError).length;

  /** 每个源的前三条地址，作为抓取结果的快速预览。 */
  const previews = new Map<string, string[]>();
  for (const entry of entries) {
    const list = previews.get(entry.source) ?? [];
    if (list.length < 3) list.push(`${entry.host}:${entry.port}`);
    previews.set(entry.source, list);
  }

  return (
    <section className="shrink-0 border-b border-line bg-panel/60">
      <div className="h-9 flex items-center gap-2 px-4">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          className="flex items-center gap-1.5 text-[12px] text-muted hover:text-text whitespace-nowrap"
        >
          <svg
            width="10"
            height="10"
            viewBox="0 0 10 10"
            className={open ? "rotate-90 transition-transform" : "transition-transform"}
            fill="currentColor"
          >
            <path d="M3 1.5 7.5 5 3 8.5z" />
          </svg>
          优选源 / 地址池
        </button>
        <span className="num text-[12px] text-text">{entries.length}</span>
        <span className="text-[12px] text-dim whitespace-nowrap">
          条地址 · 已去重 {duplicates} · {sources.length} 个源
        </span>
        <span className="ml-2 text-[12px] text-dim hidden lg:inline whitespace-nowrap">
          {sources.length === 0 ? "先刷新源清单，再抓取选中源" : "上次抓取 " + formatFetchedAt(fetchedAt)}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            onClick={() => setOpen(!open)}
            className="text-[12px] text-muted hover:text-accent px-2 h-6 border border-line rounded-[3px] bg-panel whitespace-nowrap"
          >
            {open ? "收起" : "展开编辑"}
          </button>
        </div>
      </div>

      {open ? (
        <div className="px-4 pb-3 grid grid-cols-1 xl:grid-cols-2 gap-3 items-stretch">
          <div className="border border-line rounded-[3px] bg-panel overflow-hidden flex flex-col">
            <div className="h-7 shrink-0 flex items-center justify-between px-2 border-b border-line bg-panel-2/70 text-[12px] text-dim">
              <span className="whitespace-nowrap">优选源 txt 地址</span>
              <span className="whitespace-nowrap">每行一个</span>
            </div>
            <textarea
              spellCheck={false}
              value={lastInput.sourceUrls}
              onChange={(e) => patch({ lastInput: { sourceUrls: e.target.value } })}
              className="num w-full h-[68px] bg-transparent p-2 text-[12px] leading-5 text-text outline-none resize-none placeholder:text-dim/80"
              placeholder={
                "https://bestcf.pages.dev/domain/all.txt\nhttps://bestcf.pages.dev/domain/mini.txt\nhttps://bestcf.pages.dev/vps789/top10.txt"
              }
            />
            <div className="h-7 shrink-0 flex items-center justify-between px-2 border-y border-line bg-panel-2/70 text-[12px] text-dim">
              <span className="whitespace-nowrap">手动补充优选地址</span>
              <span className="whitespace-nowrap">换行或分号分隔</span>
            </div>
            <textarea
              spellCheck={false}
              value={lastInput.manualAddresses}
              onChange={(e) => patch({ lastInput: { manualAddresses: e.target.value } })}
              className="num w-full h-[52px] bg-transparent p-2 text-[12px] leading-5 text-text outline-none resize-none placeholder:text-dim/80"
              placeholder={
                "162.159.198.1:8443#EDT导航优选域名 | BestCF.pages.dev\nyg6.ygkkk.dpdns.org:443#VPS789优选"
              }
            />
            <div className="shrink-0 flex items-center gap-2 px-2 py-2 border-t border-line bg-panel-2/40">
              <Btn
                variant="primary"
                onClick={() =>
                  void fetchPool(lastInput.manualAddresses, lastInput.sourceUrls)
                }
                className="h-7 px-2.5"
              >
                {fetching ? "抓取中…" : "抓取选中源"}
              </Btn>
              <Btn
                variant="secondary"
                onClick={() => void refreshCatalog()}
                className="h-7 px-2.5"
              >
                {refreshing ? "刷新中…" : "刷新源清单"}
              </Btn>
              <span className="ml-auto num text-[12px] text-dim whitespace-nowrap">
                上次抓取 {formatFetchedAt(fetchedAt)}
              </span>
            </div>
            {error ? (
              <div className="px-2 py-1.5 border-t border-line text-[12px] text-danger">
                {error}
              </div>
            ) : null}
          </div>

          <div className="border border-line rounded-[3px] bg-panel overflow-hidden flex flex-col">
            <div className="h-7 shrink-0 flex items-center justify-between px-2 border-b border-line bg-panel-2/70 text-[12px] text-dim">
              <span className="whitespace-nowrap">已抓取的优选源</span>
              <span className="num whitespace-nowrap">
                {checkedCount} / {sources.length} 勾选
                {failedCount > 0 ? ` · ${failedCount} 个异常` : ""}
              </span>
            </div>
            <div className="p-1.5 flex-1 min-h-[96px] max-h-[220px] overflow-auto flex flex-col gap-1">
              {sources.length === 0 ? (
                <div className="px-1.5 py-3 text-[12px] text-dim">
                  还没有优选源。点「刷新源清单」从 bestcf 页面获取，或直接在左侧粘贴地址后抓取。
                </div>
              ) : null}
              {sources.map((source) => {
                const failed = Boolean(source.lastError);
                const preview = previews.get(source.name) ?? [];
                return (
                  <div
                    key={source.url}
                    className={cn(
                      "flex items-start gap-2 px-1.5 py-1 rounded-[2px] border",
                      failed
                        ? "border-danger/30 bg-danger/[0.04]"
                        : source.checked
                          ? "border-accent/20 bg-accent-soft/60"
                          : "border-line bg-panel",
                    )}
                  >
                    <button
                      type="button"
                      role="checkbox"
                      aria-checked={source.checked}
                      aria-label={`勾选 ${source.name}`}
                      onClick={() => void toggleSource(source.url, !source.checked)}
                      className={cn(
                        "mt-0.5 size-3.5 shrink-0 grid place-items-center border rounded-[2px] leading-none",
                        source.checked
                          ? "border-accent bg-accent text-white"
                          : "border-line-2 text-transparent hover:border-muted",
                      )}
                    >
                      <svg width="10" height="10" viewBox="0 0 12 12" fill="none" aria-hidden>
                        <path
                          d="M2.6 6.3 4.7 8.4 9.4 3.6"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        />
                      </svg>
                    </button>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline gap-2">
                        <span
                          className={cn(
                            "text-[12px] num truncate",
                            source.broken ? "text-dim line-through" : "text-text",
                          )}
                          title={source.url}
                        >
                          {source.name}
                        </span>
                        <span
                          className={cn(
                            "num text-[12px] shrink-0",
                            failed ? "text-danger" : "text-dim",
                          )}
                        >
                          {source.entryCount}
                        </span>
                        {source.broken ? (
                          <span className="shrink-0 text-[12px] text-warn">更新异常</span>
                        ) : null}
                      </div>
                      <div className="num text-[12px] text-dim truncate">
                        {failed
                          ? source.lastError
                          : preview.length > 0
                            ? preview.join(" · ")
                            : "尚无条目"}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      ) : null}
    </section>
  );
}
