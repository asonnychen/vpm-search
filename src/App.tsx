import { useEffect, useRef, useState } from "react";
import ConfigBar from "./components/ConfigBar";
import LogPanel from "./components/LogPanel";
import PoolPanel from "./components/PoolPanel";
import ResultsTable from "./components/ResultsTable";
import TopBar from "./components/TopBar";
import { Dot } from "./components/ui";

export default function App() {
  const [configOpen, setConfigOpen] = useState(true);
  const [logCollapsed, setLogCollapsed] = useState(false);
  const [logHeight, setLogHeight] = useState(214);
  const dragRef = useRef<{ y: number; h: number } | null>(null);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      const next = dragRef.current.h + (dragRef.current.y - e.clientY);
      const max = Math.round(window.innerHeight * 0.6);
      setLogHeight(Math.min(max, Math.max(80, next)));
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
  }, []);

  const startDrag = (e: React.MouseEvent) => {
    dragRef.current = { y: e.clientY, h: logHeight };
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  };

  return (
    <div className="h-full flex flex-col overflow-hidden bg-canvas">
      <TopBar />
      <ConfigBar open={configOpen} onToggle={() => setConfigOpen((v) => !v)} />
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
        style={{ height: logCollapsed ? "auto" : logHeight }}
        className="shrink-0 flex flex-col min-h-0"
      >
        <LogPanel
          collapsed={logCollapsed}
          onToggleCollapse={() => setLogCollapsed((v) => !v)}
        />
      </div>

      <footer className="h-7 shrink-0 flex items-center gap-3 px-4 border-t border-line bg-panel text-[12px] text-dim">
        <span className="flex items-center gap-1.5">
          <Dot tone="ok" /> 就绪
        </span>
        <span className="text-line-2">|</span>
        <span>探测直连（已绕过系统代理）</span>
        <span className="text-line-2">|</span>
        <span className="num">%APPDATA%\vpm-search\logs</span>
        <span className="ml-auto num">
          日志 3 类 · 保留 30 天 · 数据库 12.4 MB
        </span>
      </footer>
    </div>
  );
}
