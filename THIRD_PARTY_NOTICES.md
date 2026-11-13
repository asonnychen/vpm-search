# 第三方组件许可声明

本应用（vpm-search）随安装包分发以下第三方组件。安装包与免安装包中均包含本文件。

## sing-box 1.14.4

- 用途：作为 sidecar（随包内置的独立子进程）提供 vless / vmess / trojan / ss 等代理协议，测试经真实代理链路进行。
- 许可：GNU General Public License v3.0（GPL-3.0）。
- 项目主页：<https://github.com/SagerNet/sing-box>
- 许可全文：<https://github.com/SagerNet/sing-box/blob/dev/LICENSE>
- 分发形式：未修改的官方 Windows amd64 发行版可执行文件（`sing-box.exe`，来源 `https://github.com/SagerNet/sing-box/releases/tag/v1.14.4`）。
- 说明：sing-box 以独立可执行文件形式随包分发、由本应用以子进程方式调用，本应用自身代码不因此受 GPL-3.0 约束；如需 sing-box 源码，请从上述官方仓库获取。

## DB-IP Country Lite（MMDB 离线归属地库）

- 用途：离线查询出口 IP 的国家/地区，避免运行时依赖在线接口。
- 数据来源：<https://db-ip.com/db/download/ip-to-country-lite>
- 许可：Creative Commons Attribution 4.0 International（CC BY 4.0），<https://creativecommons.org/licenses/by/4.0/>
- 署名要求：**IP Geolocation by DB-IP**（<https://db-ip.com>）
- 分发形式：`geoip/dbip-country-lite.mmdb`，随应用资源目录打包。

## 其他依赖

前端与 Rust 依赖的许可信息见 `package.json`、`pnpm-lock.yaml`、`src-tauri/Cargo.toml` 与 `src-tauri/Cargo.lock` 对应上游仓库。
