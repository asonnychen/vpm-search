# 内置 sing-box 作为 sidecar，而非在 Rust 里实现代理协议

测试必须经真实代理链路才能拿到出口 IP 与速度，而优选地址只提供 `host:port`，协议与凭据来自节点模板。自己用 Rust 实现 vless/trojan 尚可，但 vmess 的 AEAD、hysteria2 的 QUIC 都要重写一遍，且每加一种协议就是一轮新坑。选择随安装包内置 sing-box 可执行文件，用临时配置为每个并发槽开一个 `mixed` 入站，测完即销毁。

代价是安装包增大约 25MB、多一个子进程、引入 GPL-3.0 依赖；换来的是协议覆盖与稳定性，以及后续加协议零成本。曾考虑用 mihomo（同为 GPL，配置更重）与 Xray-core（MPL-2.0，但多入站编排更繁琐），sing-box 单文件静态编译、配置模型最简洁，故选它。
