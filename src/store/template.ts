import type { NodeTemplate } from "./settings";

/** 凭据脱敏后的占位符，与后端日志脱敏保持一致。 */
export const MASK = "****";

/** 支持的协议类型与传输方式，与 Rust 侧一致。 */
export const TEMPLATE_KINDS = ["vless", "vmess", "trojan", "ss"] as const;
export const TEMPLATE_TRANSPORTS = ["ws", "grpc", "h2", "tcp", "http"] as const;

/** 空值保持为空，非空凭据一律显示 `****`。 */
export function maskSecret(value: string): string {
  return value ? MASK : "";
}

/**
 * 当前选中的模板。选中项被删除或 id 不认识时回落到第一条，
 * 界面永远不会出现「有模板但没选中」的状态。
 */
export function activeTemplate(
  templates: NodeTemplate[],
  activeId: string,
): NodeTemplate | undefined {
  return templates.find((template) => template.id === activeId) ?? templates[0];
}

export type TemplateColumns = {
  kind: string;
  transport: string;
  tls: string;
};

/**
 * 表格的「类型 / 传输 / TLS」三列显示值。
 * 数据仍来自优选域名池，这里只决定三列跟着哪个模板变。
 */
export function templateColumns(template: NodeTemplate | undefined): TemplateColumns {
  if (!template) {
    return { kind: "—", transport: "—", tls: "—" };
  }
  return {
    kind: template.kind.toUpperCase(),
    transport: template.transport.toUpperCase(),
    tls: template.tls ? "TLS" : "无",
  };
}

/** 列表里的一行摘要：只出现脱敏后的凭据。 */
export function templateSummary(template: NodeTemplate): string {
  const columns = templateColumns(template);
  const parts = [columns.kind, columns.transport, columns.tls];
  if (template.uuid) parts.push(`uuid ${maskSecret(template.uuid)}`);
  if (template.password) parts.push(`password ${maskSecret(template.password)}`);
  if (template.sni) parts.push(`SNI ${template.sni}`);
  return parts.join(" · ");
}

export type TemplateSelection = {
  templates: NodeTemplate[];
  activeTemplateId: string;
};

/** 重命名；空名称视为无效输入，原样返回。 */
export function renameTemplate(
  templates: NodeTemplate[],
  id: string,
  name: string,
): NodeTemplate[] {
  const trimmed = name.trim();
  if (!trimmed) return templates;
  return templates.map((template) =>
    template.id === id ? { ...template, name: trimmed } : template,
  );
}

/** 删除模板；删掉当前模板时回落到剩下的第一条。 */
export function deleteTemplate(
  templates: NodeTemplate[],
  activeTemplateId: string,
  id: string,
): TemplateSelection {
  const next = templates.filter((template) => template.id !== id);
  const stillExists = next.some((template) => template.id === activeTemplateId);
  return {
    templates: next,
    activeTemplateId: stillExists ? activeTemplateId : (next[0]?.id ?? ""),
  };
}

/** 新增模板并立即切换过去。 */
export function addTemplate(
  templates: NodeTemplate[],
  _activeTemplateId: string,
  template: NodeTemplate,
): TemplateSelection {
  return {
    templates: [...templates, template],
    activeTemplateId: template.id,
  };
}
