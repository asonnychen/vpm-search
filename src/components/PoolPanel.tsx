import { sources } from "../mock";
import { selectLastInput, useSettingsStore } from "../store/settings";
import { cn } from "./ui";

export default function PoolPanel() {
  const lastInput = useSettingsStore(selectLastInput);
  const open = useSettingsStore((state) => state.settings.ui.poolOpen);
  const patch = useSettingsStore((state) => state.patch);
  const setOpen = (next: boolean) => patch({ ui: { poolOpen: next } });

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
        <span className="num text-[12px] text-text">128</span>
        <span className="text-[12px] text-dim whitespace-nowrap">
          条地址 · 已去重 6 · 9 个源
        </span>
        <span className="ml-2 text-[12px] text-dim hidden lg:inline whitespace-nowrap">
          粘贴优选源 txt 地址，或直接补地址
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
          </div>

          <div className="border border-line rounded-[3px] bg-panel overflow-hidden flex flex-col">
            <div className="h-7 shrink-0 flex items-center justify-between px-2 border-b border-line bg-panel-2/70 text-[12px] text-dim">
              <span className="whitespace-nowrap">已抓取的优选源</span>
              <span className="num whitespace-nowrap">9 / 11 勾选</span>
            </div>
            <div className="p-1.5 flex-1 overflow-auto flex flex-wrap content-start gap-1">
              {sources.map((s) => (
                <span
                  key={s.name}
                  className={cn(
                    "inline-flex items-center gap-1.5 h-6 px-2 rounded-[2px] border text-[12px] num whitespace-nowrap",
                    s.broken
                      ? "border-line text-dim line-through bg-panel-2/50"
                      : "border-accent/25 text-text bg-accent-soft",
                  )}
                >
                  <span
                    className={cn(
                      "inline-block size-2 rounded-[1px] border",
                      s.broken
                        ? "border-line-2"
                        : s.checked
                          ? "border-accent bg-accent"
                          : "border-line-2 bg-transparent",
                    )}
                  />
                  {s.name}
                  <span className={cn("text-[12px]", s.broken ? "text-dim" : "text-accent")}>
                    {s.count || "—"}
                  </span>
                </span>
              ))}
            </div>
          </div>
        </div>
      ) : null}
    </section>
  );
}
