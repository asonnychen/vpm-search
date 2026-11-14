//! 直连边缘探测：把探测主机名静态解析到优选地址，TLS 与 Host 仍是探测主机名。
//!
//! 这样既不需要任何节点凭据，也能拿到「这条地址能不能连、连上有多快」的结论。
//! 代价是拿不到真实出口 IP——归属地只代表地址本身（见 ADR-0007）。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use super::batch::{aggregate, BatchProber};
use super::speed::{download_streams, SpeedOptions, SpeedProber};
use super::{
    classify, resolve_host, Attempt, Endpoint, HostResolver, ProbeOptions, ProbeOutcome, Prober,
    REQUEST_TIMEOUT_S,
};
use super::{LATENCY_SAMPLES, PROBE_HOST, PROBE_URL};
use crate::logs::LogWriter;

/// 真探测器：每次探测临时建一个客户端，把探测主机名钉到目标地址。
pub struct DirectProber {
    writer: Arc<LogWriter>,
}

impl DirectProber {
    pub fn new(writer: Arc<LogWriter>) -> Self {
        Self { writer }
    }

    /// 建一个客户端：`no_proxy()` + 把 `speed.cloudflare.com` 解析到目标地址。
    fn client(
        &self,
        ip: IpAddr,
        port: u16,
        connect_timeout: std::time::Duration,
        timeout: std::time::Duration,
    ) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            // 显式禁用系统代理：探测必须走本机网络栈（ADR-0004）。
            .no_proxy()
            .connect_timeout(connect_timeout)
            .timeout(timeout)
            // 每次探测都重新建连，避免连接复用把握手时间从延迟里抹掉。
            .pool_max_idle_per_host(0)
            // TLS SNI 与 Host 仍是探测主机名，TCP 却落在优选地址上。
            .resolve(PROBE_HOST, SocketAddr::new(ip, port))
            .build()
            .map_err(|e| format!("创建探测客户端失败：{e}"))
    }

    /// 一次探测：连不上记失败，连上但没在超时内响应记超时。
    fn attempt(&self, client: &reqwest::blocking::Client) -> Attempt {
        let started = Instant::now();
        match client.get(PROBE_URL).send() {
            Ok(response) if response.status().is_success() => {
                Attempt::responded(started.elapsed().as_millis() as u64)
            }
            Ok(response) => Attempt::connect_failed(format!(
                "探测端点返回 HTTP {}",
                response.status().as_u16()
            )),
            Err(error) if error.is_timeout() => Attempt::no_response(format!(
                "{REQUEST_TIMEOUT_S}s 内未收到响应：{error}"
            )),
            Err(error) if error.is_connect() => {
                Attempt::connect_failed(format!("连接失败：{error}"))
            }
            Err(error) => Attempt::connect_failed(format!("请求失败：{error}")),
        }
    }

    /// 探测日志：一行一次尝试，只记地址与测量值。
    fn log(&self, endpoint: &Endpoint, ip: IpAddr, outcome: &ProbeOutcome) {
        let detail = match (outcome.status.as_str(), outcome.latency_ms, &outcome.error) {
            ("ok", Some(ms), _) => format!("{ms}ms"),
            (status, _, Some(error)) => format!("{status} {error}"),
            (status, _, None) => status.to_string(),
        };
        let _ = self.writer.append_probe_line(&format!(
            "{}:{} -> {ip} {detail}",
            endpoint.host, endpoint.port
        ));
    }
}

impl Prober for DirectProber {
    fn measure_latency(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome {
        let ip = match resolve_host(&endpoint.host) {
            Ok(ip) => ip,
            Err(error) => return ProbeOutcome::fail(error),
        };
        let client = match self.client(
            ip,
            endpoint.port,
            options.connect_timeout,
            options.request_timeout,
        ) {
            Ok(client) => client,
            Err(error) => return ProbeOutcome::fail(error),
        };

        let mut samples = Vec::with_capacity(LATENCY_SAMPLES);
        for _ in 0..LATENCY_SAMPLES {
            let outcome = classify(self.attempt(&client));
            self.log(endpoint, ip, &outcome);
            samples.push(outcome);
        }
        aggregate(&samples)
    }
}

impl BatchProber for DirectProber {
    fn sample(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome {
        self.measure_latency(endpoint, options)
    }
}

impl SpeedProber for DirectProber {
    fn speed_sample(
        &self,
        endpoint: &Endpoint,
        options: &ProbeOptions,
        speed: &SpeedOptions,
        cancel: &AtomicBool,
    ) -> Result<f64, String> {
        let ip = resolve_host(&endpoint.host)?;
        let client = self.client(ip, endpoint.port, options.connect_timeout, speed.timeout)?;
        let mbps = download_streams(&client, speed, cancel)?;
        let _ = self.writer.append_probe_line(&format!(
            "{}:{} -> {ip} speed {mbps} MB/s",
            endpoint.host, endpoint.port
        ));
        Ok(mbps)
    }
}

impl HostResolver for DirectProber {
    fn resolve(&self, endpoint: &Endpoint) -> Result<IpAddr, String> {
        resolve_host(&endpoint.host)
    }
}
