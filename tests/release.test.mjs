// 阶段 12 验收：打包与安装的对外契约（版本号、图标、随包资源、许可声明）。
// 只检查发布配置与产物描述，不触碰 Rust 内部实现。
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { inflateSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel) => readFileSync(path.join(root, rel), "utf8");
const readJson = (rel) => JSON.parse(read(rel));

// 极简 PNG 解码：只支持 8 位 RGB/RGBA 无隔行，足够算平均色。
function averageColor(rel) {
  const buf = readFileSync(path.join(root, rel));
  let off = 8;
  let width = 0;
  let height = 0;
  let colorType = 0;
  const idat = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      colorType = data[9];
    }
    if (type === "IDAT") idat.push(data);
    off += 12 + len;
  }
  const bpp = colorType === 6 ? 4 : 3;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));
  let prev = Buffer.alloc(stride);
  const sum = [0, 0, 0];
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const cur = Buffer.alloc(stride);
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? cur[x - bpp] : 0;
      const b = prev[x];
      const c = x >= bpp ? prev[x - bpp] : 0;
      let v = line[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      cur[x] = v & 255;
    }
    for (let x = 0; x < stride; x += bpp) {
      sum[0] += cur[x];
      sum[1] += cur[x + 1];
      sum[2] += cur[x + 2];
    }
    prev = cur;
  }
  const n = width * height;
  return sum.map((v) => v / n);
}

test("版本号在包描述、Tauri 配置、Cargo 清单三处一致", () => {
  const pkg = readJson("package.json");
  const conf = readJson("src-tauri/tauri.conf.json");
  const cargo = read("src-tauri/Cargo.toml");
  const cargoVersion = cargo.match(/^version = "([^"]+)"$/m)?.[1];

  assert.match(pkg.version, /^\d+\.\d+\.\d+$/);
  assert.equal(conf.version, pkg.version);
  assert.equal(cargoVersion, pkg.version);
});

test("打包目标包含 NSIS 安装包", () => {
  const conf = readJson("src-tauri/tauri.conf.json");
  assert.ok(Array.isArray(conf.bundle.targets));
  assert.ok(conf.bundle.targets.includes("nsis"));
});

test("图标由应用源图生成，且配置指向的每个图标都存在", () => {
  const conf = readJson("src-tauri/tauri.conf.json");
  const icons = conf.bundle.icon;
  assert.ok(icons.length > 0);
  for (const rel of icons) {
    assert.ok(existsSync(path.join(root, "src-tauri", rel)), `缺少图标 ${rel}`);
  }
  assert.ok(existsSync(path.join(root, "src-tauri/icons/icon.ico")));

  // 平均色与源图一致，说明图标确实由 app-icon.png 生成，而不是 Tauri 默认图标。
  const source = averageColor("src-tauri/icons/app-icon.png");
  for (const rel of ["icons/32x32.png", "icons/128x128.png", "icons/icon.png"]) {
    const generated = averageColor(path.join("src-tauri", rel));
    generated.forEach((v, i) => {
      assert.ok(
        Math.abs(v - source[i]) < 4,
        `${rel} 的通道 ${i} 平均色 ${v.toFixed(1)} 偏离源图 ${source[i].toFixed(1)}`,
      );
    });
  }
});

test("第三方许可声明随安装包分发", () => {
  const conf = readJson("src-tauri/tauri.conf.json");
  const resources = conf.bundle.resources ?? {};
  const mapped = Object.values(resources).map((v) => String(v).toLowerCase());
  assert.ok(
    mapped.some((v) => v.endsWith("third_party_notices.md")),
    "THIRD_PARTY_NOTICES.md 未作为 bundle resource 声明",
  );

  // NSIS 安装界面直接读取 licenseFile，必须指向随包许可声明。
  assert.equal(conf.bundle.licenseFile, "../THIRD_PARTY_NOTICES.md");
  assert.ok(existsSync(path.join(root, "THIRD_PARTY_NOTICES.md")));
  assert.match(conf.bundle.license, /CC BY 4\.0/);

  const notices = read("THIRD_PARTY_NOTICES.md");
  // 不再内置任何代理内核，声明里也不该再出现 sing-box。
  assert.doesNotMatch(notices, /sing-box/i);
  assert.match(notices, /DB-IP Country Lite/);
  assert.match(notices, /CC BY 4\.0/);
  assert.match(notices, /IP Geolocation by DB-IP/);
});

test("免安装 zip 打包脚本与 README 命令就位", () => {
  assert.ok(existsSync(path.join(root, "scripts/package-portable.ps1")));
  const script = read("scripts/package-portable.ps1");
  assert.match(script, /geoip/);
  // 免安装包只带主程序与离线库，不再复制代理内核。
  assert.doesNotMatch(script, /sing-box/i);

  const pkg = readJson("package.json");
  assert.ok(pkg.scripts["package:portable"], "缺少 package:portable 脚本");
  assert.match(
    pkg.scripts["build:release"],
    /package-portable\.ps1/,
    "build:release 必须同时产出免安装 zip",
  );

  const readme = read("README.md");
  assert.match(readme, /pnpm tauri build/);
  assert.match(readme, /package:portable/);
  assert.match(readme, /THIRD_PARTY_NOTICES\.md/);
});
