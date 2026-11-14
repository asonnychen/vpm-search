**Status: superseded by [ADR-0007](0007-direct-edge-probing.md)**

# 归属地查出口 IP，而不是优选地址

优选地址是 Cloudflare 的 anycast 边缘，直接查它的归属地永远得到"美国 / Cloudflare"，没有任何信息量。真正有意义的是这条线路落地在哪里。因此归属地查询分两步：先经代理请求 `api.ipify.org` 与 `cdn-cgi/trace` 拿到出口 IP（双源交叉取多数），再对出口 IP 做地理查询。

查询优先用内置 DB-IP Country Lite（8MB、CC BY 4.0、免 key、含中文），城市级信息与中文地名再以 `ipwho.is` 兜底，国家码到中英名的映射走本地静态表以统一繁简。ip-api.com 免费档明确禁止商用且仅 HTTP，只作为用户手动开启的批量模式，不作默认链路。

Cloudflare Workers/EDT 类落地的出口仍在 CF 网内，只能给出机房三字码而非真实城市；这是数据本身的限制，UI 用 `Cloudflare / Cloudflare / LAX` 如实表达，不硬编城市名。
