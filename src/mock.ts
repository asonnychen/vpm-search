export type RowStatus = "ok" | "timeout" | "fail";

export type Row = {
  id: string;
  type: string;
  alias: string;
  host: string;
  port: number;
  transport: string;
  geoZh: string;
  geoEn: string;
  colo: string;
  tls: boolean;
  source: string;
  latency: number | null;
  speed: number | null;
  status: RowStatus;
};

export const MAX_LATENCY = 200;

export const rows: Row[] = [
  {
    id: "1",
    type: "vless",
    alias: "EDT导航优选域名 | BestCF.pages.dev",
    host: "162.159.198.1",
    port: 8443,
    transport: "ws",
    geoZh: "加拿大",
    geoEn: "Canada",
    colo: "",
    tls: true,
    source: "BestCF·domain/all.txt",
    latency: 38,
    speed: 12.4,
    status: "ok",
  },
  {
    id: "2",
    type: "trojan",
    alias: "VPS789优选域名 TOP10 | 11-13 13:12",
    host: "yg6.ygkkk.dpdns.org",
    port: 443,
    transport: "ws",
    geoZh: "美国",
    geoEn: "United States",
    colo: "LAX",
    tls: true,
    source: "BestCF·vps789/top10.txt",
    latency: 52,
    speed: 9.8,
    status: "ok",
  },
  {
    id: "3",
    type: "vless",
    alias: "VPS789优选 | cdn.555586.xyz",
    host: "cdn.555586.xyz",
    port: 443,
    transport: "ws",
    geoZh: "日本",
    geoEn: "Japan",
    colo: "NRT",
    tls: true,
    source: "BestCF·vps789/top20.txt",
    latency: 88,
    speed: 5.6,
    status: "ok",
  },
  {
    id: "4",
    type: "vless",
    alias: "企业域名 | 迪卡侬",
    host: "www.aneko.ink",
    port: 443,
    transport: "ws",
    geoZh: "中国香港",
    geoEn: "Hong Kong",
    colo: "HKG",
    tls: true,
    source: "BestCF·domain/mini.txt",
    latency: 45,
    speed: 7.3,
    status: "ok",
  },
  {
    id: "5",
    type: "vless",
    alias: "精选域名 迷你版",
    host: "cmcc.vvvv.ee",
    port: 8443,
    transport: "ws",
    geoZh: "新加坡",
    geoEn: "Singapore",
    colo: "SIN",
    tls: true,
    source: "BestCF·domain/mini.txt",
    latency: 66,
    speed: 4.1,
    status: "ok",
  },
  {
    id: "6",
    type: "vless",
    alias: "VPS789优选 | www.wto.org",
    host: "www.wto.org",
    port: 443,
    transport: "ws",
    geoZh: "德国",
    geoEn: "Germany",
    colo: "FRA",
    tls: true,
    source: "BestCF·vps789/top10.txt",
    latency: 121,
    speed: 2.2,
    status: "ok",
  },
  {
    id: "7",
    type: "vless",
    alias: "VPS789优选 | cf.xreak.top",
    host: "cf.xreak.top",
    port: 443,
    transport: "ws",
    geoZh: "法国",
    geoEn: "France",
    colo: "CDG",
    tls: true,
    source: "BestCF·vps789/top10.txt",
    latency: 214,
    speed: 1.1,
    status: "ok",
  },
  {
    id: "8",
    type: "vless",
    alias: "官方入口 | ZeroTrust",
    host: "162.159.197.1",
    port: 443,
    transport: "ws",
    geoZh: "荷兰",
    geoEn: "Netherlands",
    colo: "AMS",
    tls: true,
    source: "BestCF·domain/all.txt",
    latency: 96,
    speed: 6.7,
    status: "ok",
  },
  {
    id: "9",
    type: "vless",
    alias: "VPS789优选 | mail.notion.com",
    host: "mail.notion.com",
    port: 443,
    transport: "ws",
    geoZh: "",
    geoEn: "",
    colo: "",
    tls: true,
    source: "BestCF·vps789/top10.txt",
    latency: null,
    speed: null,
    status: "timeout",
  },
  {
    id: "10",
    type: "vless",
    alias: "VPS789优选 | da.mfa.gov.ua",
    host: "da.mfa.gov.ua",
    port: 443,
    transport: "ws",
    geoZh: "",
    geoEn: "",
    colo: "",
    tls: true,
    source: "BestCF·vps789/top10.txt",
    latency: null,
    speed: null,
    status: "fail",
  },
];

export const sources = [
  { name: "BestCF·domain/all.txt", count: 83, checked: true, broken: false },
  { name: "BestCF·domain/mini.txt", count: 16, checked: true, broken: false },
  { name: "BestCF·domain/Domain-Asia.txt", count: 24, checked: true, broken: false },
  { name: "BestCF·vps789/top10.txt", count: 10, checked: true, broken: false },
  { name: "BestCF·vps789/top20.txt", count: 20, checked: true, broken: false },
  { name: "BestCF·ygkkk/all.txt", count: 41, checked: true, broken: false },
  { name: "BestCF·qms/all.txt", count: 12, checked: true, broken: false },
  { name: "BestCF·senflare/all.txt", count: 18, checked: true, broken: false },
  { name: "BestCF·ircf/all.txt", count: 9, checked: true, broken: false },
  { name: "BestCF·luoli/all.txt", count: 0, checked: false, broken: true },
  { name: "BestCF·kristi/all.txt", count: 0, checked: false, broken: true },
];

export type LogLevel = "INFO" | "OK" | "WARN" | "ERROR";

export type LogLine = { time: string; level: LogLevel; text: string };

export const opLogs: LogLine[] = [
  { time: "12:03:41.118", level: "INFO", text: "抓取 9 个优选源，解析 233 条，去重后 128 条" },
  { time: "12:03:41.402", level: "OK", text: "节点模板「我的 VLESS-WS」校验通过：vless · ws · tls · sni=edt.example.workers.dev" },
  { time: "12:03:41.510", level: "INFO", text: "sing-box 启动，入站 20000-20009，runId=20261113-1203" },
  { time: "12:03:42.044", level: "OK", text: "162.159.198.1:8443 延迟 38ms（3 次取最小：41/38/44）" },
  { time: "12:03:42.061", level: "OK", text: "yg6.ygkkk.dpdns.org:443 延迟 52ms（3 次取最小）" },
  { time: "12:03:42.077", level: "OK", text: "cdn.555586.xyz:443 延迟 88ms" },
  { time: "12:03:52.310", level: "WARN", text: "mail.notion.com:443 超时（连接已建立，10s 内无完整响应）" },
  { time: "12:03:52.318", level: "ERROR", text: "da.mfa.gov.ua:443 失败：tls handshake timeout after 5000ms" },
  { time: "12:03:52.640", level: "OK", text: "162.159.198.1:8443 测速 12.4 MB/s（4 流 · 10MB · 8.1s）" },
  { time: "12:03:52.901", level: "INFO", text: "出口 IP 校验：api.ipify.org=104.16.3.7，cdn-cgi/trace ip=104.16.3.7 → 一致" },
  { time: "12:03:53.220", level: "OK", text: "本轮完成：成功 8 · 超时 1 · 失败 1 · 用时 11.7s" },
  { time: "12:04:02.005", level: "INFO", text: "日志清理任务：应用日志保留 30 天，删除 0 个文件" },
];

export const appLogs: LogLine[] = [
  { time: "12:03:41.118", level: "INFO", text: '{"event":"source.fetch","traceId":"20261113-1203","sources":9,"entries":233,"dedup":128,"durationMs":284}' },
  { time: "12:03:41.402", level: "INFO", text: '{"event":"template.validate","traceId":"20261113-1203","type":"vless","transport":"ws","tls":true,"uuid":"****"}' },
  { time: "12:03:41.510", level: "INFO", text: '{"event":"sidecar.start","traceId":"20261113-1203","inbounds":[20000,20009],"pid":18472}' },
  { time: "12:03:42.044", level: "INFO", text: '{"event":"probe.delay","spanId":"a3f1","target":"162.159.198.1:8443","samples":[41,38,44],"durationMs":38}' },
  { time: "12:03:52.310", level: "WARN", text: '{"event":"probe.delay","spanId":"b7c2","target":"mail.notion.com:443","error":"response timeout","elapsedMs":10000}' },
  { time: "12:03:52.318", level: "ERROR", text: '{"event":"probe.delay","spanId":"c9d0","target":"da.mfa.gov.ua:443","error":"tls handshake timeout","stack":"at connect (probe.rs:118)","elapsedMs":5000}' },
  { time: "12:03:52.901", level: "INFO", text: '{"event":"geo.lookup","spanId":"a3f1","exitIp":"104.16.3.7","asn":13335,"country":"CA","colo":"","source":"dbip+ipwhois"}' },
  { time: "12:03:53.220", level: "INFO", text: '{"event":"run.finish","traceId":"20261113-1203","ok":8,"timeout":1,"fail":1,"durationMs":11710}' },
];

export const coreLogs: LogLine[] = [
  { time: "12:03:41.507", level: "INFO", text: "sing-box 1.12.4 (go1.24) windows/amd64" },
  { time: "12:03:41.508", level: "INFO", text: "inbound/mixed[in-0]: listening at 127.0.0.1:20000" },
  { time: "12:03:41.509", level: "INFO", text: "outbound/vless[out-0]: 162.159.198.1:8443 via ws" },
  { time: "12:03:41.622", level: "INFO", text: "inbound/mixed[in-1]: connection to 104.18.32.7:443" },
  { time: "12:03:42.180", level: "WARN", text: "outbound/vless[out-5]: handshake timeout after 5s" },
  { time: "12:03:52.340", level: "INFO", text: "sing-box stopped, 9 inbounds closed" },
];
