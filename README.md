# vpm-search

自用的 Cloudflare 优选地址测速工具：从优选源收集地址，直连 Cloudflare 边缘测延迟与速度、查地址本身的归属地，并导出可直接粘贴给客户端的地址列表。

技术栈：Tauri v2 + React + TypeScript + SQLite。**无需节点凭据**：不内置代理内核，也不需要 vless / vmess / trojan / ss 的 UUID 或密码。

## 探测方式

每个目标先解析出 IP（域名走系统 DNS，IP 直接用），再把探测主机名 `speed.cloudflare.com`
静态解析到 `(目标 IP, 目标端口)`：TLS SNI 与 Host 都是探测主机名，TCP 连接却落在优选地址上。
这样既不需要任何凭据，也能量出「这条地址能不能连、连上有多快」。

- 延迟：对 `https://speed.cloudflare.com/__down?bytes=0` 请求三次取最小值，单次总超时 10 秒，连接超时沿用设置。
- 测速：4 条并发流各 2.5MB（合计 10MB）、总时长上限 15 秒，换算成 MB/s。
- 探测请求一律显式禁用系统代理（见 ADR-0004）。

拿不到真实出口 IP 是这套方案的代价：归属地只代表**地址本身**注册在哪个国家（见 ADR-0007）。

## 归属地的离线库

归属地优先用内置的 DB-IP Country Lite（MMDB，约 8MB，CC BY 4.0，免 key，含国家码）。
文件随应用打包（`bundle.resources` 映射到资源目录的 `geoip/`），运行时不联网也能显示国家：

```
src-tauri/resources/geoip/dbip-country-lite.mmdb
```

需要更新时执行：

```powershell
pwsh -File scripts/fetch-geoip.ps1            # 当月版本
pwsh -File scripts/fetch-geoip.ps1 -Month 2026-10
```

运行时按「主程序同目录 `geoip/` → 资源目录 `geoip/` → 开发期 `src-tauri/resources/geoip`」查找。
文件缺失或损坏时自动回落在线的 `https://ipwho.is`（HTTPS、免费 1000 次/天、允许商用）。

中英文地名统一走 `src-tauri/src/geo/countries.rs` 的本地映射表，不用在线源的中文译名，避免繁简混排。

## 数据位置

数据库与日志都在 `%APPDATA%\vpm-search\`：`data.db` 与 `logs\` 同目录。
首次启动时若发现旧路径 `%APPDATA%\com.asonnychen.vpm-search\data.db`，会复制一份到新路径并写一条应用日志；
复制失败只记录日志，不阻塞启动。

## 开发与验证

```powershell
pnpm install                 # 首次安装依赖
pnpm tauri dev               # 启动原生窗口（开发模式）
pnpm exec tsc --noEmit       # 类型检查
pnpm test                    # 前端单测 + 发布配置测试
cd src-tauri
cargo test
cargo check
```

## 打包

先确保 `src-tauri/resources/geoip/dbip-country-lite.mmdb` 在位（见上文），然后：

```powershell
pnpm build:release            # 一条命令产出 NSIS 安装包 + 免安装 zip
```

`pnpm build:release` 等价于先 `pnpm tauri build`，再执行
`pwsh -NoProfile -File scripts/package-portable.ps1 -SkipBuild`。也可以分两步跑：

```powershell
pnpm tauri build             # 生产构建 + 打包（NSIS 安装包）
pnpm package:portable        # 生成免安装 zip（需已构建主程序）
```

产物位置：

| 产物 | 路径 |
| --- | --- |
| 主程序 | `src-tauri/target/release/vpm-search.exe` |
| NSIS 安装包 | `src-tauri/target/release/bundle/nsis/vpm-search_0.1.0_x64-setup.exe` |
| 免安装 zip | `src-tauri/target/release/bundle/portable/vpm-search-0.1.0-portable.zip` |

免安装 zip 内是解压即用的目录：`vpm-search.exe`、`geoip/dbip-country-lite.mmdb`、
`THIRD_PARTY_NOTICES.md` 与 `README.md`。离线归属地库随包分发，首次运行不需要联网下载。

图标由 `src-tauri/icons/app-icon.png`（1024×1024）生成，重新生成：

```powershell
pnpm exec tauri icon src-tauri/icons/app-icon.png
```

## 目录

- `src/`：React 界面与状态（zustand），`src/store/*.test.ts` 为前端单测。
- `src-tauri/src/`：Rust 后端。`probe/` 负责直连延迟与测速，`geo/` 负责归属地，`logs/` 负责三类日志，`sources/` 负责优选源抓取，`store.rs` 负责 SQLite。
- `scripts/`：`fetch-geoip.ps1` 准备打包资源，`package-portable.ps1` 生成免安装 zip。
- `tests/`：发布配置与产物契约测试（版本号一致性、图标来源、随包许可声明）。
- `docs/adr/`：架构决策记录；`GLOSSARY.md`：领域词汇。

## 许可

应用代码为自用项目。随包分发的第三方组件及其许可全文见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)，
该文件同时作为 bundle resource 打进安装包：

- **DB-IP Country Lite**：CC BY 4.0，署名 **IP Geolocation by DB-IP**（<https://db-ip.com>）。
