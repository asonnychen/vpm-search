# vpm-search

自用的 Cloudflare 优选地址测速工具：从优选源收集地址，经真实代理链路测延迟、速度与落地归属地，并导出可直接粘贴给客户端的地址列表。

技术栈：Tauri v2 + React + TypeScript + SQLite；代理链路由随包内置的 sing-box 提供。

## 首次运行

sing-box 是 GPL-3.0 的第三方可执行文件，不进版本库。克隆后**先执行一次**下载脚本：

```powershell
pwsh -File scripts/fetch-singbox.ps1
```

脚本从 GitHub Releases 下载固定版本的 Windows amd64 压缩包，解出 `sing-box.exe` 并放到：

```
src-tauri/binaries/sing-box-x86_64-pc-windows-msvc.exe
```

该路径已被 `.gitignore` 忽略；`tauri.conf.json` 的 `bundle.externalBin` 指向 `binaries/sing-box`，打包时会自动带上对应目标三元组的文件名后缀。运行时按「主程序同目录 → 资源目录 → 开发期 `src-tauri/binaries`」的顺序查找该文件。

未放置二进制时应用照常启动，只是单条测试会返回「失败：未找到 sing-box」。

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
文件缺失或损坏时自动回落在线的 `https://ipwho.is`（HTTPS、免费 1000 次/天、允许商用）；
两者都不可用时，`cdn-cgi/trace` 的 `loc` 仍能给出国家码。

中英文地名统一走 `src-tauri/src/geo/countries.rs` 的本地映射表，不用在线源的中文译名，
避免繁简混排。出口仍在 Cloudflare 网内（ASN 13335 或落在已知 CF 段）时只显示机房三字码，
归属地两段显示 `Cloudflare`，不硬编城市名。

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

先确保 `src-tauri/binaries/sing-box-x86_64-pc-windows-msvc.exe` 与
`src-tauri/resources/geoip/dbip-country-lite.mmdb` 都在位（见上文两个下载脚本），然后：

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

免安装 zip 内是解压即用的目录：`vpm-search.exe`、内置的 `sing-box-x86_64-pc-windows-msvc.exe`、
`geoip/dbip-country-lite.mmdb`、`THIRD_PARTY_NOTICES.md` 与 `README.md`。sing-box 与离线归属地库
都随包分发，首次运行不需要联网下载。

图标由 `src-tauri/icons/app-icon.png`（1024×1024）生成，重新生成：

```powershell
pnpm exec tauri icon src-tauri/icons/app-icon.png
```

## 目录

- `src/`：React 界面与状态（zustand），`src/store/*.test.ts` 为前端单测。
- `src-tauri/src/`：Rust 后端。`probe/` 负责单条真连接延迟与测速，`geo/` 负责出口 IP 与归属地，`logs/` 负责三类日志，`sources/` 负责优选源抓取，`store.rs` 负责 SQLite。
- `scripts/`：`fetch-singbox.ps1`、`fetch-geoip.ps1` 准备打包资源，`package-portable.ps1` 生成免安装 zip。
- `tests/`：发布配置与产物契约测试（版本号一致性、图标来源、随包许可声明）。
- `docs/adr/`：架构决策记录；`GLOSSARY.md`：领域词汇。

## 许可

应用代码为自用项目。随包分发的第三方组件及其许可全文见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)，
该文件同时作为 bundle resource 打进安装包：

- **sing-box 1.14.4**：GPL-3.0，作为 sidecar 以独立可执行文件分发，来源 <https://github.com/SagerNet/sing-box>。
- **DB-IP Country Lite**：CC BY 4.0，署名 **IP Geolocation by DB-IP**（<https://db-ip.com>）。

