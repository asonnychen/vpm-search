# 第三方组件许可声明

本应用（vpm-search）随安装包分发以下第三方组件。安装包与免安装包中均包含本文件。

## DB-IP Country Lite（MMDB 离线归属地库）

- 用途：离线查询地址所属的国家/地区，避免运行时依赖在线接口。
- 数据来源：<https://db-ip.com/db/download/ip-to-country-lite>
- 许可：Creative Commons Attribution 4.0 International（CC BY 4.0），<https://creativecommons.org/licenses/by/4.0/>
- 署名要求：**IP Geolocation by DB-IP**（<https://db-ip.com>）
- 分发形式：`geoip/dbip-country-lite.mmdb`，随应用资源目录打包。

## 其他依赖

前端与 Rust 依赖的许可信息见 `package.json`、`pnpm-lock.yaml`、`src-tauri/Cargo.toml` 与 `src-tauri/Cargo.lock` 对应上游仓库。
应用不内置任何代理内核，探测请求由 Rust 侧的 HTTP 客户端直连 Cloudflare 边缘。
