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
