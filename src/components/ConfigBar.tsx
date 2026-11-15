import { Btn, Field, Num } from "./ui";
import {
  LATENCY_SAMPLES,
  REQUEST_TIMEOUT_S,
  SPEED_PARAMS,
  selectLastInput,
  selectSettings,
  useSettingsStore,
} from "../store/settings";
import { selectProbeBatch, speedTargetsFromIds, useProbeStore } from "../store/probe";
import { useSelectionStore } from "../store/selection";
import { usePoolStore } from "../store/pool";

export default function ConfigBar({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  const settings = useSettingsStore(selectSettings);
  const patch = useSettingsStore((state) => state.patch);
  const batch = useProbeStore(selectProbeBatch);
  const startBatch = useProbeStore((state) => state.startBatch);
  const startSpeedBatch = useProbeStore((state) => state.startSpeedBatch);
  const startGeoBatch = useProbeStore((state) => state.startGeoBatch);
  const cancelBatch = useProbeStore((state) => state.cancelBatch);
  const selectedIds = useSelectionStore((state) => state.ids);

  const refreshCatalog = usePoolStore((state) => state.refreshCatalog);
  const fetchPool = usePoolStore((state) => state.fetchPool);
  const refreshing = usePoolStore((state) => state.refreshing);
  const fetching = usePoolStore((state) => state.fetching);
  const lastInput = useSettingsStore(selectLastInput);

  /** 一键抓取：先刷新源清单，再抓取勾选的源。 */
  const fetchEverything = async () => {
    await refreshCatalog();
    await fetchPool(lastInput.sourceUrls);
  };

  const runningSpeed = batch.running && batch.mode === "speed";
  const runningGeo = batch.running && batch.mode === "geo";
  const speedScope = selectedIds.length > 0 ? `选中 ${selectedIds.length} 行` : "整池";

  // 抓取与刷新期间池子内容会整体换掉，测试与导出先让路，避免对着半份数据操作。
  const poolBusy = fetching || refreshing;
  const poolBusyReason = fetching ? "正在抓取优选源" : "正在刷新源清单";
  const testDisabled = batch.running || poolBusy;
  const testDisabledReason = poolBusy
    ? `${poolBusyReason}，结束后才能发起测试`
    : "测试进行中，结束后才能再发起一轮";

  const percent =
    batch.total > 0
      ? Math.min(100, Math.round((batch.completed / batch.total) * 100))
      : 0;

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
          最大延时 {settings.maxLatencyMs}ms · 并发 {settings.concurrency} · 超时{" "}
          {settings.connectTimeoutS}s/{REQUEST_TIMEOUT_S}s
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <Btn
            variant="secondary"
            disabled={poolBusy || batch.running}
            onClick={() => void fetchEverything()}
            title={
              batch.running
                ? "测试进行中，结束后才能抓取优选源"
                : poolBusy
                  ? `${poolBusyReason}，请等待完成`
                  : "刷新源清单并抓取勾选的优选源"
            }
          >
            {refreshing ? "刷新中…" : fetching ? "抓取中…" : "抓取优选源"}
          </Btn>
          <Btn
            variant="primary"
            disabled={testDisabled}
            title={testDisabled ? testDisabledReason : "对整池地址跑一轮真连接延迟"}
            onClick={() => void startBatch()}
          >
            {batch.running && batch.mode === "latency" ? "测试中…" : "一键测试真连接延迟"}
          </Btn>
          <Btn
            variant="accent"
            disabled={testDisabled}
            title={
              testDisabled
                ? testDisabledReason
                : `对${speedScope}跑一轮多流测速，不影响已有延迟`
            }
            onClick={() =>
              void startSpeedBatch(
                selectedIds.length > 0 ? speedTargetsFromIds(selectedIds) : undefined,
              )
            }
          >
            {runningSpeed ? "测速中…" : "一键多线程测速"}
          </Btn>
          <Btn
            variant="secondary"
            disabled={testDisabled}
            title={
              testDisabled
                ? testDisabledReason
                : `查${speedScope}地址本身的归属地，不影响已有延迟与速度`
            }
            onClick={() =>
              void startGeoBatch(
                selectedIds.length > 0 ? speedTargetsFromIds(selectedIds) : undefined,
              )
            }
          >
            {runningGeo ? "查询中…" : "查询归属地"}
          </Btn>
          <Btn
            variant="danger"
            disabled={!batch.running || batch.stopping}
            title={batch.running ? "停止后已完成的保留，未开始的标记为未测" : "当前没有正在跑的测试"}
            onClick={() => void cancelBatch()}
          >
            {batch.stopping ? "停止中…" : "停止"}
          </Btn>
          <Btn
            variant="secondary"
            disabled={poolBusy}
            title={poolBusy ? `${poolBusyReason}，结束后才能导出` : "在表格工具栏导出当前结果"}
          >
            导出 txt
          </Btn>
        </div>
      </div>

      {batch.running || batch.stopping ? (
        <div className="flex items-center gap-3 px-4 py-1.5 border-t border-line bg-panel-2/40 text-[12px] text-dim">
          <span className="whitespace-nowrap">
            {batch.stopping
              ? "正在停止，已开始的会跑完"
              : batch.mode === "speed"
                ? "批量测速进行中"
                : batch.mode === "geo"
                  ? "归属地查询进行中"
                  : "批量测试进行中"}
          </span>
          <span
            className="h-1.5 flex-1 max-w-md bg-canvas ring-1 ring-line rounded-full overflow-hidden"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={batch.total}
            aria-valuenow={batch.completed}
          >
            <span
              className="block h-full bg-accent rounded-full transition-[width] duration-200"
              style={{ width: `${percent}%` }}
            />
          </span>
          <span className="num text-text w-10 text-right">{percent}%</span>
          <span className="num whitespace-nowrap">
            已完成 {batch.completed} / {batch.total}
          </span>
          {batch.runId ? (
            <span className="num text-dim whitespace-nowrap">run {batch.runId}</span>
          ) : null}
        </div>
      ) : null}

      {open ? (
        <div className="px-4 pb-3 pt-1 flex flex-wrap items-end gap-x-4 gap-y-3">
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
              max={10}
              onChange={(connectTimeoutS) => patch({ connectTimeoutS })}
            />
          </Field>
          <Field label="请求总超时">
            <div className="flex items-center gap-1.5 whitespace-nowrap">
              <div
                className="h-8 w-[76px] flex items-center gap-1 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted"
                title={`固定 ${REQUEST_TIMEOUT_S} 秒，不可修改`}
              >
                <span className="num text-text">{REQUEST_TIMEOUT_S}</span>
                <span className="num text-dim">s</span>
              </div>
              <span className="text-[12px] text-dim" title="固定值，不可修改">
                固定
              </span>
            </div>
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
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted whitespace-nowrap">
              <span className="num text-text">{LATENCY_SAMPLES}</span>
              <span>次取最小</span>
            </div>
          </Field>
          <Field label="测速">
            <div className="h-8 flex items-center gap-1.5 px-2 border border-line bg-panel-2 rounded-[3px] text-[12px] text-muted whitespace-nowrap">
              <span className="num text-text">{SPEED_PARAMS.streams}</span>
              <span>流 ·</span>
              <span className="num text-text">{SPEED_PARAMS.sizeMb}</span>
              <span>MB ·</span>
              <span className="num text-text">{SPEED_PARAMS.timeoutS}</span>
              <span>s</span>
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
