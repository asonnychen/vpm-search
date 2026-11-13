import { useEffect, useRef } from "react";
import ConfigBar from "./components/ConfigBar";
import LogPanel from "./components/LogPanel";
import PoolPanel from "./components/PoolPanel";
import ResultsTable from "./components/ResultsTable";
import TopBar from "./components/TopBar";
import { Dot } from "./components/ui";
import { selectSettings, selectUi, useSettingsStore } from "./store/settings";
import { usePoolStore } from "./store/pool";
import { useLogsStore } from "./store/logs";
import { subscribeProbeProgress, useProbeStore } from "./store/probe";

export default function App() {
  const settings = useSettingsStore(selectSettings);
  const ui = useSettingsStore(selectUi);
  const saveError = useSettingsStore((state) => state.saveError);
  const load = useSettingsStore((state) => state.load);
  const patch = useSettingsStore((state) => state.patch);
  const dragRef = useRef<{ y: number; h: number } | null>(null);

  // 启动时读取本地设置，回填配置、列宽与优选源文本。
  useEffect(() => {
    void load();
  }, [load]);

  // 读取上次的池；首次使用还没有源清单时自动抓一次 bestcf 页面。
  useEffect(() => {
    void (async () => {
      await usePoolStore.getState().load();
      if (usePoolStore.getState().sources.length === 0) {
        await usePoolStore.getState().refreshCatalog();
      }
    })();
  }, []);

  // 关窗/刷新前把待保存的修改立即落库，尽量不丢最后一次编辑。
  useEffect(() => {
    const flush = () => {
      void useSettingsStore.getState().flush();
    };
    window.addEventListener("pagehide", flush);
    return () => window.removeEventListener("pagehide", flush);
  }, []);

  // 启动时读回最近若干行日志，并订阅后端推送的新日志。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let active = true;
    void useLogsStore.getState().load();
    void useLogsStore
      .getState()
      .subscribe()
      .then((fn) => {
        if (active) unlisten = fn;
        else fn();
      });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // 订阅后端推来的批量进度（已完成 / 总数）。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let active = true;
    void subscribeProbeProgress((progress) => {
      useProbeStore.getState().applyProgress(progress);
    }).then((fn) => {
      if (active) unlisten = fn;
      else fn();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      const next = dragRef.current.h + (dragRef.current.y - e.clientY);
      const max = Math.round(window.innerHeight * 0.6);
      patch({ ui: { logHeight: Math.min(max, Math.max(80, next)) } });
    };
    const onUp = () => {
      dragRef.current = null;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [patch]);

  const startDrag = (e: React.MouseEvent) => {
    dragRef.current = { y: e.clientY, h: ui.logHeight };
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  };

  return (
    <div className="h-full flex flex-col overflow-hidden bg-canvas">
      <TopBar />
      <ConfigBar
        open={ui.configOpen}
        onToggle={() => patch({ ui: { configOpen: !ui.configOpen } })}
      />
      <PoolPanel />
      <ResultsTable />

      <div
        onMouseDown={startDrag}
        className="h-1.5 shrink-0 cursor-row-resize group relative"
        title="拖动调整日志高度"
      >
        <div className="absolute inset-x-0 top-1/2 h-px -translate-y-1/2 bg-line group-hover:bg-accent-dim" />
      </div>

      <div
        style={{ height: ui.logCollapsed ? "auto" : ui.logHeight }}
        className="shrink-0 flex flex-col min-h-0"
      >
        <LogPanel
          collapsed={ui.logCollapsed}
          onToggleCollapse={() => patch({ ui: { logCollapsed: !ui.logCollapsed } })}
        />
      </div>

      <footer className="h-7 shrink-0 flex items-center gap-3 px-4 border-t border-line bg-panel text-[12px] text-dim">
        <span className="flex items-center gap-1.5">
          <Dot tone={saveError ? "err" : "ok"} /> {saveError ? "设置未保存" : "就绪"}
        </span>
        <span className="text-line-2">|</span>
        <span>探测直连（已绕过系统代理）</span>
        <span className="text-line-2">|</span>
        <span className="num">%APPDATA%\com.asonnychen.vpm-search\logs</span>
        <span className="ml-auto num">
          日志 3 类 · 保留 {settings.logRetention.appDays} 天 · 数据库 12.4 MB
        </span>
      </footer>
    </div>
  );
}
