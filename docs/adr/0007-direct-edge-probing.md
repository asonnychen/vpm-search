# 直连 Cloudflare 边缘探测，而不是经本地代理隧道

**Status: accepted（取代 [ADR-0001](0001-bundle-sing-box-sidecar.md) 与 [ADR-0002](0002-geolocate-exit-ip-not-endpoint.md)）**

## 背景

ADR-0001 让应用随包内置 sing-box，用节点模板（协议 + 凭据）为每条优选地址开一条临时隧道，再经隧道请求 204 端点。ADR-0002 在此之上经隧道取出口 IP，再对出口 IP 查归属地。

这套方案的代价在实际使用中超过了收益：

- 安装包多出约 25MB，还要引入 GPL-3.0 依赖与一个子进程；
- 用户必须先有一条可用的节点凭据（UUID / 密码 / 传输 / SNI），否则一条都测不了；
- 模板与 worker 对不上时只会得到 403，排查成本高；
- 归属地查的是出口 IP，而优选地址池里的地址绝大多数落在 Cloudflare 网内，出口几乎总是同一批 CF 机房，信息量很低。

## 决定

去掉节点模板与 sing-box sidecar，改为**直连探测**：

1. 每个目标先解析出 IP（域名走系统 DNS，IP 直接用）。
2. 用 reqwest 建客户端，把探测主机名 `speed.cloudflare.com` 静态解析到 `(目标 IP, 目标端口)`。于是 TLS SNI 与 Host 都是 `speed.cloudflare.com`，证书校验照常通过，TCP 连接却落在优选地址上——这正是「这条地址能不能连」的答案。
3. 延迟：对 `https://speed.cloudflare.com/__down?bytes=0` 请求三次取最小值，单次总超时 10 秒，连接超时沿用设置。
4. 测速：4 条并发流各 2.5MB（合计 10MB），总时长上限 15 秒，换算 MB/s。
5. 探测请求一律 `no_proxy()`（ADR-0004 仍然有效）。
6. 归属地查**地址本身**的 IP：离线 DB-IP 优先，`ipwho.is` 兜底，只保留中文名与英文名。

## 代价

- **拿不到真实出口 IP**：不再有「这条线路落地在哪里」的信息，归属地只说明地址本身注册在哪个国家。对 Cloudflare anycast 地址来说这一列的信息量有限，但它是可验证的事实，不假装知道更多。
- 测速端点固定为 `speed.cloudflare.com`：如果某个地址只放行特定 SNI，这里会判失败——这同样是有意义的结论。
- 不再支持任意代理协议：本工具的目的从「测代理线路」变成「测优选地址到 CF 边缘的连通性与吞吐」。

## 收益

- 安装包与免安装 zip 明显变小，去掉 GPL-3.0 依赖与子进程。
- 无需任何节点凭据，抓完优选源就能直接测。
- 探测路径只有「系统 DNS → TCP → TLS → HTTP」，失败原因可直接归因。

## 影响

- 删除 `src-tauri/src/template/**`、`probe/singbox.rs`、`probe/slot.rs`、`probe/port.rs`、`probe/config.rs`、`geo/exit.rs`、`scripts/fetch-singbox.ps1`、`bundle.externalBin`。
- `test_result` 的 `exit_ip` 与 `colo` 列不再读写；旧库里的这两列保留不动（不 DROP），新库不再创建它们。
- 表格删除「类型 / 传输 / TLS / 机房」四列。
- 日志的第三类从「sing-box 日志」改为「探测日志」，每次探测尝试落一行原文。
