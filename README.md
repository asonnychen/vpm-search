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

## 开发与验证

```powershell
pnpm install
pnpm exec tsc --noEmit
pnpm test
cd src-tauri
cargo test
cargo check
```

## 目录

- `src/`：React 界面与状态（zustand），`src/store/*.test.ts` 为前端单测。
- `src-tauri/src/`：Rust 后端。`probe/` 负责单条真连接延迟，`logs/` 负责三类日志，`sources/` 负责优选源抓取，`store.rs` 负责 SQLite。
- `docs/adr/`：架构决策记录；`GLOSSARY.md`：领域词汇。

## 许可

应用代码为自用项目；随包分发的 sing-box 遵循 GPL-3.0，见其官方仓库。
