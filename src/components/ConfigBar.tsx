import { useState } from "react";
import { Btn, Field, Num } from "./ui";
import TemplatePanel, { type TemplatePanelMode } from "./TemplatePanel";
import {
  selectNodeTemplates,
  selectSettings,
  selectSpeedParams,
  useSettingsStore,
} from "../store/settings";
import { activeTemplate, deleteTemplate } from "../store/template";

export default function ConfigBar({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  const settings = useSettingsStore(selectSettings);
  const templates = useSettingsStore(selectNodeTemplates);
  const speed = useSettingsStore(selectSpeedParams);
  const patch = useSettingsStore((state) => state.patch);
  const [panel, setPanel] = useState<TemplatePanelMode | null>(null);

  const selected = activeTemplate(templates, settings.activeTemplateId);

  const removeSelected = () => {
    if (!selected) return;
    const next = deleteTemplate(templates, settings.activeTemplateId, selected.id);
    patch({
      nodeTemplates: next.templates,
      activeTemplateId: next.activeTemplateId,
    });
  };

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
          {selected?.name ?? "未选择模板"} · 最大延时 {settings.maxLatencyMs}ms · 并发{" "}
          {settings.concurrency} · 超时 {settings.connectTimeoutS}s/
          {settings.requestTimeoutS}s
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
          <Field label="节点模板" width="300px">
            <div className="flex items-center gap-1">
              <div className="relative flex-1">
                <select
                  value={selected?.id ?? ""}
                  onChange={(event) => {
                    if (event.target.value === "__new__") {
                      setPanel("new");
                      return;
                    }
                    patch({ activeTemplateId: event.target.value });
                  }}
                  className="appearance-none w-full h-8 bg-panel border border-line rounded-[3px] px-2 pr-7 text-[12px] outline-none focus:border-accent-dim"
                >
                  {templates.length === 0 ? <option value="">未选择模板</option> : null}
                  {templates.map((template) => (
                    <option key={template.id} value={template.id}>
                      {template.name}
                    </option>
                  ))}
                  <option value="__new__">+ 新建模板…</option>
                </select>
                <span className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-dim text-[12px]">
                  ▼
                </span>
              </div>
              <Btn variant="ghost" title="新建模板" onClick={() => setPanel("new")}>
                + 新建
              </Btn>
              <Btn
                variant="ghost"
                title="重命名当前模板"
                onClick={() => setPanel("rename")}
              >
                重命名
              </Btn>
              <Btn variant="danger" title="删除当前模板" onClick={removeSelected}>
                删除
              </Btn>
            </div>
          </Field>

          <Field label="最大延时" width="100px">
            <Num
              value={settings.maxLatencyMs}
              unit="ms"
              width="100px"
              min={1}
              max={10_000}
              onChange={(maxLatencyMs) => patch({ maxLatencyMs })}
            />
          </Field>
          <Field label="连接超时" width="92px">
            <Num
              value={settings.connectTimeoutS}
              unit="s"
              width="92px"
              min={1}
              max={60}
              onChange={(connectTimeoutS) => patch({ connectTimeoutS })}
            />
          </Field>
          <Field label="请求总超时" width="92px">
            <Num
              value={settings.requestTimeoutS}
              unit="s"
              width="92px"
              min={1}
              max={120}
              onChange={(requestTimeoutS) => patch({ requestTimeoutS })}
            />
          </Field>
          <Field label="地址并发" width="86px">
            <Num
              value={settings.concurrency}
              width="86px"
              min={1}
              max={32}
              onChange={(concurrency) => patch({ concurrency })}
            />
          </Field>

          <div className="hidden xl:block h-8 w-px bg-line mx-1" />

          <Field label="延迟采样">
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted">
              <span className="num text-text">{settings.latencySamples}</span> 次取最小
            </div>
          </Field>
          <Field label="测速">
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted">
              <span className="num text-text">{speed.streams}</span> 流 ·
              <span className="num text-text">{speed.sizeMb}</span> MB ·
              <span className="num text-text">{speed.timeoutS}</span> s
            </div>
          </Field>

          <div className="ml-auto flex items-center gap-2 text-[12px] text-dim">
            <span className="num">9</span> 个源已勾选
            <span className="text-line-2">|</span>
            <span className="num">128</span> 条待测
          </div>
        </div>
      ) : null}

      {panel ? (
        <TemplatePanel
          key={panel}
          initialMode={panel}
          onClose={() => setPanel(null)}
        />
      ) : null}
    </section>
  );
}
