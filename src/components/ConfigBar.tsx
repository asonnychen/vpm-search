import { Btn, Field, Num } from "./ui";

export default function ConfigBar({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <section className="shrink-0 border-b border-line bg-panel/40">
      <div className="min-h-9 flex flex-wrap items-center gap-x-2 gap-y-1.5 px-4 py-1.5">
        <button
          type="button"
          onClick={onToggle}
          className="flex items-center gap-1.5 text-[12px] text-muted hover:text-text"
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
          测试配置
        </button>
        <span className="text-[12px] text-dim num hidden sm:inline">
          我的 VLESS-WS · 最大延时 200ms · 并发 10 · 超时 5s/10s
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <Btn variant="secondary">抓取优选源</Btn>
          <Btn variant="primary">一键测试真连接延迟</Btn>
          <Btn variant="accent">一键多线程测速</Btn>
          <Btn variant="danger">停止</Btn>
          <Btn variant="secondary">导出 txt</Btn>
        </div>
      </div>

      {open ? (
        <div className="px-4 pb-3 pt-1 flex flex-wrap items-end gap-x-4 gap-y-3">
          <Field label="节点模板" width="210px">
            <div className="relative">
              <select className="appearance-none w-full h-8 bg-panel border border-line rounded-[3px] px-2 pr-7 text-[12px] outline-none focus:border-accent-dim">
                <option>我的 VLESS-WS</option>
                <option>备用 Trojan-CDN</option>
                <option>+ 新建模板…</option>
              </select>
              <span className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-dim text-[12px]">
                ▼
              </span>
            </div>
          </Field>

          <Field label="最大延时" width="100px">
            <Num value={200} unit="ms" width="100px" />
          </Field>
          <Field label="连接超时" width="92px">
            <Num value={5} unit="s" width="92px" />
          </Field>
          <Field label="请求总超时" width="92px">
            <Num value={10} unit="s" width="92px" />
          </Field>
          <Field label="地址并发" width="86px">
            <Num value={10} width="86px" />
          </Field>

          <div className="hidden xl:block h-8 w-px bg-line mx-1" />

          <Field label="延迟采样">
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted">
              <span className="num text-text">3</span> 次取最小
            </div>
          </Field>
          <Field label="测速">
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted">
              <span className="num text-text">4</span> 流 ·
              <span className="num text-text">10</span> MB ·
              <span className="num text-text">15</span> s
            </div>
          </Field>

          <div className="ml-auto flex items-center gap-2 text-[12px] text-dim">
            <span className="num">9</span> 个源已勾选
            <span className="text-line-2">|</span>
            <span className="num">128</span> 条待测
          </div>
        </div>
      ) : null}
    </section>
  );
}
