import { describe, expect, it, beforeEach } from "vitest";
import { useSelectionStore } from "./selection";
import { speedTargetsFromIds } from "./probe";

describe("选中行", () => {
  beforeEach(() => {
    useSelectionStore.getState().clear();
  });

  it("切换选中与取消选中", () => {
    const store = useSelectionStore.getState();
    store.toggle("104.16.1.1:8443");
    store.toggle("104.16.2.2:2053");
    expect(useSelectionStore.getState().ids).toEqual([
      "104.16.1.1:8443",
      "104.16.2.2:2053",
    ]);

    useSelectionStore.getState().toggle("104.16.1.1:8443");
    expect(useSelectionStore.getState().ids).toEqual(["104.16.2.2:2053"]);
  });

  it("选中行能拆成测速目标", () => {
    expect(speedTargetsFromIds(["104.16.1.1:8443", "例子.com:2053"])).toEqual([
      { host: "104.16.1.1", port: 8443 },
      { host: "例子.com", port: 2053 },
    ]);
  });

  it("坏键被丢弃", () => {
    expect(speedTargetsFromIds(["没有端口", ":8443", "a:0", "a:99999"])).toEqual([]);
  });
});
