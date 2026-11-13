import { Dot, Divider } from "./ui";

export default function TopBar() {
  return (
    <header className="h-12 shrink-0 flex items-center gap-3 px-4 border-b border-line bg-panel relative z-10">
      <div className="flex items-center gap-2.5">
        <svg width="26" height="26" viewBox="0 0 26 26" fill="none" aria-hidden>
          <rect
            x="0.5"
            y="0.5"
            width="25"
            height="25"
            rx="4"
            stroke="var(--color-line-2)"
          />
          <path
            d="M6 17.5 10 12l3.4 4.1L16.5 9l3.5 8.5"
            stroke="var(--color-accent)"
            strokeWidth="1.6"
            strokeLinecap="square"
          />
          <circle cx="10" cy="12" r="1.6" fill="var(--color-accent)" />
        </svg>
        <div className="leading-none">
          <div className="text-[14px] font-semibold tracking-tight">
            vpm-search
            <span className="ml-2 num text-[12px] text-dim font-normal">
              v0.1.0
            </span>
          </div>
          <div className="mt-1 text-[12px]  text-dim font-mono">
            cloudflare 优选地址测速终端
          </div>
        </div>
      </div>

      <Divider />

      <div className="hidden md:flex items-center gap-2 text-[12px] text-muted">
        <Dot tone="ok" />
        <span>sing-box 1.12.4 就绪</span>
        <span className="text-dim">·</span>
        <span className="num text-dim">run 20261113-1203</span>
      </div>

      <div className="ml-auto flex items-center gap-2">
        <span className="hidden lg:inline-flex items-center gap-1.5 h-7 px-2 border border-line rounded-[3px] text-[12px] text-muted bg-panel-2/60">
          <Dot tone="ok" />
          系统代理已绕过
        </span>
        <span className="inline-flex items-center gap-1.5 h-7 px-2 border border-line rounded-[3px] text-[12px] text-muted bg-panel-2/60">
          <Dot tone="ok" />
          DB 12.4 MB
        </span>
        <button
          type="button"
          className="size-7 grid place-items-center border border-line rounded-[3px] text-dim hover:text-text hover:border-line-2"
          title="设置"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
          </svg>
        </button>
      </div>
    </header>
  );
}
