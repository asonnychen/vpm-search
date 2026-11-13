import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Btn, Field, cn } from "./ui";
import { selectNodeTemplates, useSettingsStore, type NodeTemplate } from "../store/settings";
import {
  TEMPLATE_KINDS,
  TEMPLATE_TRANSPORTS,
  deleteTemplate,
  renameTemplate,
  templateSummary,
} from "../store/template";
import { isTauriRuntime } from "../store/utils";

export type TemplatePanelMode = "list" | "new" | "rename";

function newId(): string {
  return `tpl-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

function blankTemplate(): NodeTemplate {
  return {
    id: newId(),
    name: "",
    kind: "vless",
    server: "",
    port: 443,
    uuid: "",
    password: "",
    method: "",
    transport: "ws",
    tls: true,
    sni: "",
    host: "",
    path: "/",
    serviceName: "",
  };
}

const inputClass =
  "h-8 w-full bg-panel border border-line rounded-[3px] px-2 text-[12px] outline-none focus:border-accent-dim placeholder:text-dim/80";

/** 节点模板管理：列表、切换、重命名、删除与新建（粘贴链接或手填）。 */
export default function TemplatePanel({
  initialMode,
  onClose,
}: {
  initialMode: TemplatePanelMode;
  onClose: () => void;
}) {
  const templates = useSettingsStore(selectNodeTemplates);
  const activeId = useSettingsStore((state) => state.settings.activeTemplateId);
  const patch = useSettingsStore((state) => state.patch);

  const [form, setForm] = useState<NodeTemplate | null>(
    initialMode === "new" ? blankTemplate() : null,
  );
  const [renaming, setRenaming] = useState<string | null>(
    initialMode === "rename" ? activeId : null,
  );
  const [draftName, setDraftName] = useState(
    initialMode === "rename"
      ? (templates.find((template) => template.id === activeId)?.name ?? "")
      : "",
  );
  const [link, setLink] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [parsing, setParsing] = useState(false);

  const setField = <K extends keyof NodeTemplate>(key: K, value: NodeTemplate[K]) => {
    setForm((current) => (current ? { ...current, [key]: value } : current));
  };

  const parseLink = async () => {
    setError(null);
    if (!link.trim()) {
      setError("请先粘贴一条分享链接");
      return;
    }
    if (!isTauriRuntime()) {
      setError("浏览器原型里无法解析链接，请在应用窗口内使用");
      return;
    }
    setParsing(true);
    try {
      const parsed = await invoke<NodeTemplate>("parse_node_link", { link });
      setForm({
        ...parsed,
        name: parsed.name.trim() || `新模板 ${templates.length + 1}`,
      });
    } catch (cause) {
      // 后端返回的是可读中文原因；解析失败不会产生半成品模板。
      setError(typeof cause === "string" ? cause : String(cause));
    } finally {
      setParsing(false);
    }
  };

  const save = () => {
    if (!form) return;
    const name = form.name.trim();
    const server = form.server.trim();
    if (!name) {
      setError("模板名称不能为空");
      return;
    }
    if (!server) {
      setError("服务器地址不能为空");
      return;
    }
    if (!Number.isInteger(form.port) || form.port < 1 || form.port > 65535) {
      setError("端口必须在 1–65535 之间");
      return;
    }
    const created: NodeTemplate = { ...form, name, server };
    patch({
      nodeTemplates: [...templates, created],
      activeTemplateId: created.id,
    });
    onClose();
  };

  const commitRename = (id: string) => {
    if (!draftName.trim()) {
      setError("模板名称不能为空");
      return;
    }
    patch({ nodeTemplates: renameTemplate(templates, id, draftName) });
    setRenaming(null);
    setError(null);
  };

  const remove = (id: string) => {
    const next = deleteTemplate(templates, activeId, id);
    patch({
      nodeTemplates: next.templates,
      activeTemplateId: next.activeTemplateId,
    });
    if (renaming === id) setRenaming(null);
  };

  const showUuid = form?.kind === "vless" || form?.kind === "vmess";
  const showPassword = form?.kind === "trojan" || form?.kind === "ss";
  const showMethod = form?.kind === "ss";

  return (
    <div className="px-4 pb-3 pt-2 border-t border-line bg-panel-2/40">
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <span className="text-[12px] text-muted">节点模板</span>
        <span className="num text-[12px] text-dim">
          {templates.length} 条 · 凭据已脱敏
        </span>
        <div className="ml-auto flex items-center gap-1">
          <Btn variant="ghost" onClick={() => setForm(blankTemplate())}>
            + 新建
          </Btn>
          <Btn variant="ghost" onClick={onClose}>
            收起
          </Btn>
        </div>
      </div>

      <ul className="flex flex-col gap-1">
        {templates.map((template) => {
          const isActive = template.id === activeId;
          return (
            <li
              key={template.id}
              className={cn(
                "flex min-h-8 items-center gap-2 rounded-[3px] border px-2 py-1",
                isActive ? "border-accent/40 bg-accent-soft" : "border-line bg-panel",
              )}
            >
              <button
                type="button"
                onClick={() => patch({ activeTemplateId: template.id })}
                className="min-w-0 flex-1 text-left"
                title="切换到此模板"
              >
                <span className="text-[12px] text-text">{template.name}</span>
                <span className="ml-2 text-[12px] text-dim">{templateSummary(template)}</span>
              </button>

              {renaming === template.id ? (
                <div className="flex items-center gap-1">
                  <input
                    value={draftName}
                    autoFocus
                    onChange={(event) => setDraftName(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") commitRename(template.id);
                      if (event.key === "Escape") setRenaming(null);
                    }}
                    className={cn(inputClass, "h-6 w-40")}
                  />
                  <Btn variant="secondary" onClick={() => commitRename(template.id)}>
                    保存
                  </Btn>
                  <Btn variant="ghost" onClick={() => setRenaming(null)}>
                    取消
                  </Btn>
                </div>
              ) : (
                <div className="flex items-center gap-1">
                  <Btn
                    variant="ghost"
                    onClick={() => {
                      setRenaming(template.id);
                      setDraftName(template.name);
                      setError(null);
                    }}
                  >
                    重命名
                  </Btn>
                  <Btn variant="danger" onClick={() => remove(template.id)}>
                    删除
                  </Btn>
                </div>
              )}
            </li>
          );
        })}
        {templates.length === 0 ? (
          <li className="text-[12px] text-dim">
            还没有模板。点「+ 新建」粘贴一条 vless / vmess / trojan / ss 链接。
          </li>
        ) : null}
      </ul>

      {form ? (
        <div className="mt-3 border-t border-line pt-2">
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <span className="text-[12px] text-muted">新建模板</span>
            <span className="text-[12px] text-dim">
              粘贴分享链接解析，或直接手填关键字段
            </span>
            <Btn variant="ghost" className="ml-auto" onClick={() => setForm(null)}>
              取消
            </Btn>
          </div>

          <div className="flex flex-wrap items-end gap-x-3 gap-y-2">
            <Field label="分享链接" width="520px">
              <div className="flex gap-1">
                <input
                  value={link}
                  onChange={(event) => setLink(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") void parseLink();
                  }}
                  placeholder="vless://… / vmess://… / trojan://… / ss://…"
                  className={inputClass}
                />
                <Btn variant="secondary" onClick={() => void parseLink()}>
                  {parsing ? "解析中…" : "解析"}
                </Btn>
              </div>
            </Field>

            <Field label="名称" width="180px">
              <input
                value={form.name}
                onChange={(event) => setField("name", event.target.value)}
                placeholder="例如 主力 VLESS"
                className={inputClass}
              />
            </Field>
            <Field label="类型" width="104px">
              <select
                value={form.kind}
                onChange={(event) => setField("kind", event.target.value)}
                className={inputClass}
              >
                {TEMPLATE_KINDS.map((kind) => (
                  <option key={kind} value={kind}>
                    {kind.toUpperCase()}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="传输" width="104px">
              <select
                value={form.transport}
                onChange={(event) => setField("transport", event.target.value)}
                className={inputClass}
              >
                {TEMPLATE_TRANSPORTS.map((transport) => (
                  <option key={transport} value={transport}>
                    {transport.toUpperCase()}
                  </option>
                ))}
              </select>
            </Field>
            <label className="flex h-8 items-center gap-1.5 text-[12px] text-muted">
              <input
                type="checkbox"
                checked={form.tls}
                onChange={(event) => setField("tls", event.target.checked)}
              />
              TLS
            </label>

            <Field label="服务器" width="180px">
              <input
                value={form.server}
                onChange={(event) => setField("server", event.target.value)}
                placeholder="测试时会被替换"
                className={inputClass}
              />
            </Field>
            <Field label="端口" width="88px">
              <input
                value={form.port}
                inputMode="numeric"
                onChange={(event) => {
                  const parsed = Number(event.target.value);
                  setField("port", Number.isFinite(parsed) ? Math.round(parsed) : 0);
                }}
                className={cn(inputClass, "num")}
              />
            </Field>
            <Field label="SNI" width="160px">
              <input
                value={form.sni}
                onChange={(event) => setField("sni", event.target.value)}
                className={inputClass}
              />
            </Field>
            <Field label="Host" width="160px">
              <input
                value={form.host}
                onChange={(event) => setField("host", event.target.value)}
                className={inputClass}
              />
            </Field>
            <Field label="Path" width="140px">
              <input
                value={form.path}
                onChange={(event) => setField("path", event.target.value)}
                className={inputClass}
              />
            </Field>
            <Field label="serviceName" width="140px">
              <input
                value={form.serviceName}
                onChange={(event) => setField("serviceName", event.target.value)}
                className={inputClass}
              />
            </Field>

            {showUuid ? (
              <Field label="UUID" width="300px">
                <input
                  value={form.uuid}
                  onChange={(event) => setField("uuid", event.target.value)}
                  placeholder="保存原文，列表与日志显示 ****"
                  className={cn(inputClass, "num")}
                />
              </Field>
            ) : null}
            {showPassword ? (
              <Field label="密码" width="180px">
                <input
                  type="password"
                  value={form.password}
                  onChange={(event) => setField("password", event.target.value)}
                  placeholder="保存原文，列表与日志显示 ****"
                  className={inputClass}
                />
              </Field>
            ) : null}
            {showMethod ? (
              <Field label="加密方式" width="140px">
                <input
                  value={form.method}
                  onChange={(event) => setField("method", event.target.value)}
                  placeholder="aes-256-gcm"
                  className={inputClass}
                />
              </Field>
            ) : null}
          </div>

          {error ? <p className="mt-2 text-[12px] text-danger">{error}</p> : null}

          <div className="mt-2 flex justify-end gap-2">
            <Btn variant="primary" onClick={save}>
              保存模板
            </Btn>
          </div>
        </div>
      ) : error ? (
        <p className="mt-2 text-[12px] text-danger">{error}</p>
      ) : null}
    </div>
  );
}
