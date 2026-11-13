import { describe, expect, it } from "vitest";
import type { NodeTemplate } from "./settings";
import {
  activeTemplate,
  addTemplate,
  deleteTemplate,
  maskSecret,
  renameTemplate,
  templateColumns,
  templateSummary,
} from "./template";

function template(overrides: Partial<NodeTemplate> = {}): NodeTemplate {
  return {
    id: "t1",
    name: "主力 VLESS",
    kind: "vless",
    server: "cf.example.com",
    port: 443,
    uuid: "550e8400-e29b-41d4-a716-446655440000",
    password: "",
    method: "",
    transport: "ws",
    tls: true,
    sni: "cdn.example.com",
    host: "cdn.example.com",
    path: "/ws",
    serviceName: "",
    ...overrides,
  };
}

describe("模板选择器", () => {
  it("选中项命中时返回该模板", () => {
    const list = [template(), template({ id: "t2", name: "备用", kind: "trojan" })];
    expect(activeTemplate(list, "t2")?.name).toBe("备用");
  });

  it("选中项不存在时回落到第一条", () => {
    const list = [template(), template({ id: "t2", name: "备用" })];
    expect(activeTemplate(list, "已经删掉的")?.id).toBe("t1");
  });

  it("没有模板时返回 undefined", () => {
    expect(activeTemplate([], "")).toBeUndefined();
  });
});

describe("切换模板后的类型 / 传输 / TLS 列值", () => {
  it("vless + ws + tls", () => {
    expect(templateColumns(template())).toEqual({
      kind: "VLESS",
      transport: "WS",
      tls: "TLS",
    });
  });

  it("trojan + tcp 且无 TLS", () => {
    const trojan = template({ kind: "trojan", transport: "tcp", tls: false });
    expect(templateColumns(trojan)).toEqual({
      kind: "TROJAN",
      transport: "TCP",
      tls: "无",
    });
  });

  it("切换模板后三列立即换成新模板的值", () => {
    const before = templateColumns(activeTemplate([template()], "t1"));
    const after = templateColumns(
      activeTemplate(
        [template(), template({ id: "t2", kind: "ss", transport: "tcp", tls: false })],
        "t2",
      ),
    );
    expect(before.kind).toBe("VLESS");
    expect(after).toEqual({ kind: "SS", transport: "TCP", tls: "无" });
  });

  it("没有模板时三列显示占位符", () => {
    expect(templateColumns(undefined)).toEqual({
      kind: "—",
      transport: "—",
      tls: "—",
    });
  });
});

describe("模板列表的增删改", () => {
  it("重命名只改名称", () => {
    const list = [template(), template({ id: "t2", name: "备用" })];
    const renamed = renameTemplate(list, "t1", "新的名字");
    expect(renamed[0].name).toBe("新的名字");
    expect(renamed[0].uuid).toBe(list[0].uuid);
    expect(renamed[1]).toEqual(list[1]);
  });

  it("删除当前模板后回落到第一条", () => {
    const list = [template(), template({ id: "t2", name: "备用" })];
    const result = deleteTemplate(list, "t1", "t1");
    expect(result.templates).toHaveLength(1);
    expect(result.activeTemplateId).toBe("t2");
  });

  it("删除非当前模板时选中项不变", () => {
    const list = [template(), template({ id: "t2", name: "备用" })];
    const result = deleteTemplate(list, "t1", "t2");
    expect(result.activeTemplateId).toBe("t1");
  });

  it("删除最后一条后没有选中项", () => {
    const result = deleteTemplate([template()], "t1", "t1");
    expect(result.templates).toEqual([]);
    expect(result.activeTemplateId).toBe("");
  });

  it("新增模板后自动选中新模板", () => {
    const created = template({ id: "t9", name: "新模板" });
    const result = addTemplate([template()], created);
    expect(result.templates.map((item) => item.id)).toEqual(["t1", "t9"]);
    expect(result.activeTemplateId).toBe("t9");
  });
});

describe("凭据脱敏", () => {
  it("非空凭据显示 ****，空值保持为空", () => {
    expect(maskSecret("550e8400-e29b-41d4-a716-446655440000")).toBe("****");
    expect(maskSecret("hunter2")).toBe("****");
    expect(maskSecret("")).toBe("");
  });

  it("列表摘要不泄露凭据，但保留类型与传输", () => {
    const summary = templateSummary(template({ password: "hunter2" }));
    expect(summary).toContain("VLESS");
    expect(summary).toContain("WS");
    expect(summary).toContain("****");
    expect(summary).not.toContain("550e8400");
    expect(summary).not.toContain("hunter2");
  });
});
