import { useEffect, useRef } from "react";
import ConfigBar from "./components/ConfigBar";
import HistoryPanel from "./components/HistoryPanel";
import LogPanel from "./components/LogPanel";
import PoolPanel from "./components/PoolPanel";
import ResultsTable from "./components/ResultsTable";
import TopBar from "./components/TopBar";
import { Dot } from "./components/ui";
import { selectSettings, selectUi, useSettingsStore } from "./store/settings";
import { usePoolStore } from "./store/pool";
import { useLogsStore } from "./store/logs";
import { subscribeProbeProgress, subscribeProbeRow, useProbeStore } from "./store/probe";
import { formatBytes, isOverDbLimit, useHistoryStore } from "./store/history";

/** 日志栏最小高度，与拖拽下限一致。 */
const LOG_MIN_HEIGHT = 80;
/** 表格区至少保留的高度，保证工具栏、表头与底部统计都看得见。 */
const TABLE_MIN_HEIGHT = 160;
/** 分割条高度（h-1.5 = 6px）。 */
const SPLITTER_HEIGHT = 6;

export default function App() {
  const settings = useSettingsStore(selectSettings);
  const ui = useSettingsStore(selectUi);
  const saveError = useSettingsStore((state) => state.saveError);
  const load = useSettingsStore((state) => state.load);
  const patch = useSettingsStore((state) => state.patch);
  const dbBytes = useHistoryStore((state) => state.dbBytes);
  const probeRunId = useProbeStore((state) => state.batch.runId);
  const dragRef = useRef<{ y: number; h: number } | null>(null);
  /** 表格 + 日志这块可用区域，拖拽上限按它的实际高度算。 */
  const splitRef = useRef<HTMLDivElement | null>(null);
  /** 当前允许的日志最大高度，由 ResizeObserver 维护。 */
  const maxLogRef = useRef<number>(LOG_MIN_HEIGHT);

  // 启动时读取本地设置，回填配置、列宽与优选源文本。
  useEffect(() => {
    void load();
  }, [load]);

  // 数据库体积在状态栏与顶栏常驻显示。
  useEffect(() => {
    void useHistoryStore.getState().loadSize();
  }, []);

  // 每跑完一轮测试，库会变大，及时刷新顶栏体积。
  useEffect(() => {
    if (probeRunId) void useHistoryStore.getState().loadSize();
  }, [probeRunId]);

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

  // 订阅后端逐条推来的实时结果：延迟 / 速度 / 归属地立即写进表格，不等整轮结束。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let active = true;
    void subscribeProbeRow((rows) => {
      useProbeStore.getState().applyRows(rows);
    }).then((fn) => {
      if (active) unlisten = fn;
      else fn();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // 拖拽上限按表格 + 日志这块容器的实际高度算，窗口缩放时同步收紧。
  useEffect(() => {
    const node = splitRef.current;
    if (!node) return;

    const measure = () => {
      // 日志最高 = 容器高度 − 表格区最小高度 − 分割条高度。
      const max = Math.max(
        LOG_MIN_HEIGHT,
        Math.round(node.clientHeight - TABLE_MIN_HEIGHT - SPLITTER_HEIGHT),
      );
      maxLogRef.current = max;
      if (useSettingsStore.getState().settings.ui.logHeight > max) {
        patch({ ui: { logHeight: max } });
      }
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [patch]);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      const next = dragRef.current.h + (dragRef.current.y - e.clientY);
      const max = Math.max(LOG_MIN_HEIGHT, maxLogRef.current);
      patch({ ui: { logHeight: Math.min(max, Math.max(LOG_MIN_HEIGHT, next)) } });
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
      <div ref={splitRef} className="flex-1 min-h-0 flex flex-col overflow-hidden">
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
      </div>

      <HistoryPanel />

      <footer className="h-7 shrink-0 flex items-center gap-3 px-4 border-t border-line bg-panel text-[12px] text-dim">
        <span className="flex items-center gap-1.5">
          <Dot tone={saveError ? "err" : "ok"} /> {saveError ? "设置未保存" : "就绪"}
        </span>
        <span className="text-line-2">|</span>
        <span>探测直连（已绕过系统代理）</span>
        <span className="text-line-2">|</span>
        <span className="num">%APPDATA%\vpm-search\logs</span>
        <span className="ml-auto num">
          日志 3 类 · 保留 {settings.logRetention.appDays} 天 ·{" "}
          <span className={isOverDbLimit(dbBytes) ? "text-warn" : undefined}>
            数据库 {formatBytes(dbBytes)}
            {isOverDbLimit(dbBytes) ? "（超过 50MB，请清理）" : ""}
          </span>
        </span>
      </footer>
    </div>
  );
}
