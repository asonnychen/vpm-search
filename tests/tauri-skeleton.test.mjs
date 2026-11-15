// 阶段 1 验收：Tauri 骨架的对外契约（配置文件与插件注册）。
// 这些断言只检查公开配置，不触碰 Rust 内部实现细节。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel) => readFileSync(path.join(root, rel), "utf8");
const readJson = (rel) => JSON.parse(read(rel));

test("tauri 配置把前端开发服务器与构建产物接上", () => {
  const conf = readJson("src-tauri/tauri.conf.json");
  assert.equal(conf.productName, "vpm-search");
  assert.equal(conf.identifier, "com.asonnychen.vpm-search");
  assert.equal(conf.build.beforeDevCommand, "pnpm dev");
  assert.equal(conf.build.devUrl, "http://localhost:5173");
  assert.equal(conf.build.beforeBuildCommand, "pnpm build");
  assert.equal(conf.build.frontendDist, "../dist");
});

test("主窗口尺寸、可最大化与标题符合设计", () => {
  const conf = readJson("src-tauri/tauri.conf.json");
  const win = conf.app.windows[0];
  assert.equal(win.title, "vpm-search");
  assert.equal(win.width, 1280);
  assert.equal(win.height, 820);
  assert.equal(win.minWidth, 1024);
  assert.equal(win.minHeight, 680);
  assert.equal(win.maximizable, true);
  assert.equal(win.resizable, true);
});

test("单实例插件已声明并注册", () => {
  const cargo = read("src-tauri/Cargo.toml");
  assert.match(cargo, /tauri-plugin-single-instance/);
  const entry = read("src-tauri/src/lib.rs") + read("src-tauri/src/main.rs");
  assert.match(entry, /single_instance::init/);
});

test("包名与 Rust 清单一致", () => {
  const cargo = read("src-tauri/Cargo.toml");
  assert.match(cargo, /^name = "vpm-search"$/m);
  const pkg = readJson("package.json");
  assert.match(cargo, new RegExp(`^version = "${pkg.version.replace(/\./g, "\\.")}"$`, "m"));
  assert.match(pkg.version, /^\d+\.\d+\.\d+$/);
});
