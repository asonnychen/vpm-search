//! 直连边缘探测：不经过任何代理，直接对优选地址发起 TLS 请求。
//!
//! 设计要点（见 ADR-0007 与 ADR-0004）：
//! - 探测主机名 `speed.cloudflare.com` 被静态解析到「目标 IP + 目标端口」，
//!   于是 TLS SNI 与 Host 都是探测主机名，TCP 连接却落在优选地址上；
//! - 探测请求一律 `no_proxy()`，不读取系统代理；
//! - 连接阶段未建立记「失败」，连上但总超时内没有响应记「超时」。

pub mod batch;
pub mod direct;
pub mod fake;
pub mod speed;

pub use direct::DirectProber;

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 连接超时允许的秒数范围。
pub const MIN_CONNECT_TIMEOUT_S: u32 = 1;
pub const MAX_CONNECT_TIMEOUT_S: u32 = 10;

/// 单次请求的总超时，固定 10 秒，不随设置变化。
pub const REQUEST_TIMEOUT_S: u64 = 10;

/// 每条地址的延迟采样次数，取最小值。
pub const LATENCY_SAMPLES: usize = 3;

/// 探测主机名：TLS SNI 与 Host 都是它，连接却落在优选地址上。
pub const PROBE_HOST: &str = "speed.cloudflare.com";

/// 延迟探测端点；`bytes=0` 只取响应头，不计传输时间。
pub const PROBE_URL: &str = "https://speed.cloudflare.com/__down?bytes=0";

/// 把底层错误摘要收拾成可读文案（去掉首尾空白）。
pub fn describe_error(error: &str) -> String {
    error.trim().to_string()
}

/// 一条待测优选地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

/// 单次测试的结论。界面按它决定显示延迟、超时还是失败。
///
/// `Untested` 只出现在批量测试被停止时：已经开始的行保留结论，
/// 还没轮到的行落库为未测，不伪装成失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStatus {
    Ok,
    Timeout,
    Fail,
    Untested,
}

impl ProbeStatus {
    /// 落库与日志用的标识。
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::Timeout => "timeout",
            ProbeStatus::Fail => "fail",
            ProbeStatus::Untested => "untested",
        }
    }
}

/// 一条优选地址的测试结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeOutcome {
    pub status: ProbeStatus,
    /// 成功时的往返毫秒数。
    pub latency_ms: Option<u64>,
    /// 失败或超时的原因，成功时为 `None`。
    pub error: Option<String>,
}

impl ProbeOutcome {
    pub fn ok(latency_ms: u64) -> Self {
        Self {
            status: ProbeStatus::Ok,
            latency_ms: Some(latency_ms.max(1)),
            error: None,
        }
    }

    pub fn timeout(error: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Timeout,
            latency_ms: None,
            error: Some(error.into()),
        }
    }

    pub fn fail(error: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Fail,
            latency_ms: None,
            error: Some(error.into()),
        }
    }

    /// 被停止时还没开始的行。
    pub fn untested() -> Self {
        Self {
            status: ProbeStatus::Untested,
            latency_ms: None,
            error: None,
        }
    }
}

/// 一次探测尝试的原始观测，分类只依赖这三个事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// 连接阶段是否已建立（TCP 与 TLS 握手完成）。
    pub connected: bool,
    /// 收到响应的往返毫秒数；`None` 表示没有收到。
    pub responded_ms: Option<u64>,
    /// 失败原因，成功时为 `None`。
    pub error: Option<String>,
}

impl Attempt {
    pub fn responded(latency_ms: u64) -> Self {
        Self {
            connected: true,
            responded_ms: Some(latency_ms),
            error: None,
        }
    }

    pub fn connect_failed(error: impl Into<String>) -> Self {
        Self {
            connected: false,
            responded_ms: None,
            error: Some(error.into()),
        }
    }

    pub fn no_response(error: impl Into<String>) -> Self {
        Self {
            connected: true,
            responded_ms: None,
            error: Some(error.into()),
        }
    }
}

/// 按「连接是否建立」与「是否收到响应」给出结论。
pub fn classify(attempt: Attempt) -> ProbeOutcome {
    if let Some(latency_ms) = attempt.responded_ms {
        return ProbeOutcome::ok(latency_ms);
    }
    let message = describe_error(&attempt.error.unwrap_or_else(|| "未知错误".to_string()));
    if attempt.connected {
        ProbeOutcome::timeout(message)
    } else {
        ProbeOutcome::fail(message)
    }
}

/// 逐行实时事件（`probe:row`）的数据部分。
///
/// `runId` 与 `kind`（`delay` / `speed` / `geo`）由命令层补齐，这里只管一行地址的结论。
/// `colo` 是历史字段：机房已废弃，恒为 `null`，保留它只为不破坏事件契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowEvent {
    pub host: String,
    pub port: u16,
    /// `ok` / `timeout` / `fail` / `untested`。
    pub status: ProbeStatus,
    pub latency_ms: Option<u64>,
    pub speed_mbps: Option<f64>,
    pub country_zh: Option<String>,
    pub country_en: Option<String>,
    pub colo: Option<String>,
    pub error: Option<String>,
}

impl RowEvent {
    /// 只有地址与状态的事件；延迟、速度、归属地各自补自己的字段。
    pub fn new(endpoint: &Endpoint, status: ProbeStatus) -> Self {
        Self {
            host: endpoint.host.clone(),
            port: endpoint.port,
            status,
            latency_ms: None,
            speed_mbps: None,
            country_zh: None,
            country_en: None,
            colo: None,
            error: None,
        }
    }

    /// 延迟测试的一行：只有延迟与错误。
    pub fn latency(endpoint: &Endpoint, outcome: &ProbeOutcome) -> Self {
        Self {
            latency_ms: outcome.latency_ms,
            error: outcome.error.clone(),
            ..Self::new(endpoint, outcome.status)
        }
    }
}

/// 一次探测的参数。连接超时来自设置（1–10 秒），总超时固定 10 秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self::from_connect_timeout_s(5)
    }
}

impl ProbeOptions {
    /// 把设置里的秒数夹到 1–10 秒，总超时始终 10 秒。
    pub fn from_connect_timeout_s(seconds: u32) -> Self {
        let seconds = seconds.clamp(MIN_CONNECT_TIMEOUT_S, MAX_CONNECT_TIMEOUT_S);
        Self {
            connect_timeout: Duration::from_secs(seconds as u64),
            request_timeout: Duration::from_secs(REQUEST_TIMEOUT_S),
        }
    }
}

/// 把域名解析成 IP：IP 直接用，域名走系统 DNS。
pub fn resolve_host(host: &str) -> Result<IpAddr, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("地址为空".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    use std::net::ToSocketAddrs;
    let mut addresses = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| format!("解析 {host} 失败：{e}"))?;
    addresses
        .next()
        .map(|address| address.ip())
        .ok_or_else(|| format!("解析 {host} 没有得到任何 IP"))
}

/// 把域名解析成 IP 的能力：真探测器走系统 DNS，假探测器返回预设值。
pub trait HostResolver: Send + Sync {
    fn resolve(&self, endpoint: &Endpoint) -> Result<IpAddr, String>;
}

/// 系统 DNS 解析器。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn resolve(&self, endpoint: &Endpoint) -> Result<IpAddr, String> {
        resolve_host(&endpoint.host)
    }
}

/// 测延迟的统一接口。
pub trait Prober: Send + Sync {
    /// 采样 [`LATENCY_SAMPLES`] 次并取最小值。
    fn measure_latency(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome;
}

/// 一条单条测试的输入。
pub struct SingleProbe<'a> {
    pub endpoint: &'a Endpoint,
    pub alias: &'a str,
    pub options: ProbeOptions,
}

/// 跑一条地址的测试并落库：一条测试轮次 + 一条结果。
#[cfg_attr(not(test), allow(dead_code))] // 命令层走 measure_latency + persist_single
pub fn run_single(
    prober: &dyn Prober,
    store: &crate::store::Store,
    run_id: &str,
    started_at: &str,
    probe: SingleProbe<'_>,
) -> Result<ProbeOutcome, String> {
    let outcome = prober.measure_latency(probe.endpoint, &probe.options);
    persist_single(store, run_id, started_at, &probe, &outcome)?;
    Ok(outcome)
}

/// 把一次已经完成的测试写进 `test_run` 与 `test_result`。
///
/// 单独拆出来是为了让命令层「先测、后短暂加锁落库」，不长时间占着数据库锁。
pub fn persist_single(
    store: &crate::store::Store,
    run_id: &str,
    started_at: &str,
    probe: &SingleProbe<'_>,
    outcome: &ProbeOutcome,
) -> Result<(), String> {
    let finished_at = crate::logs::now_iso8601();

    // 参数快照只记协议与超时；探测不涉及任何凭据。
    let params = serde_json::json!({
        "mode": "latency",
        "connectTimeoutS": probe.options.connect_timeout.as_secs(),
        "requestTimeoutS": probe.options.request_timeout.as_secs(),
        "samples": LATENCY_SAMPLES,
    });

    store
        .insert_test_run(&crate::store::TestRunRecord {
            id: run_id,
            started_at,
            finished_at: &finished_at,
            params_json: &params.to_string(),
            total: 1,
            ok_count: u32::from(outcome.status == ProbeStatus::Ok),
            timeout_count: u32::from(outcome.status == ProbeStatus::Timeout),
            fail_count: u32::from(outcome.status == ProbeStatus::Fail),
        })
        .map_err(|e| e.to_string())?;

    store
        .insert_test_result(run_id, probe.endpoint, probe.alias, outcome)
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 收到响应记为成功并带延迟() {
        let outcome = classify(Attempt::responded(123));
        assert_eq!(outcome.status, ProbeStatus::Ok);
        assert_eq!(outcome.latency_ms, Some(123));
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn 连接阶段失败记为失败而不是超时() {
        let outcome = classify(Attempt::connect_failed("连接被拒绝"));
        assert_eq!(outcome.status, ProbeStatus::Fail);
        assert_eq!(outcome.latency_ms, None);
        assert_eq!(outcome.error.as_deref(), Some("连接被拒绝"));
    }

    #[test]
    fn 已连接但无响应记为超时() {
        let outcome = classify(Attempt::no_response("10s 内未收到响应"));
        assert_eq!(outcome.status, ProbeStatus::Timeout);
        assert_eq!(outcome.latency_ms, None);
        assert!(outcome.error.unwrap().contains("10s"));
    }

    #[test]
    fn 错误摘要只去掉首尾空白() {
        assert_eq!(describe_error("  连接被拒绝  "), "连接被拒绝");
    }

    #[test]
    fn 连接超时秒数被夹在_1_到_10_之间() {
        assert_eq!(
            ProbeOptions::from_connect_timeout_s(5).connect_timeout,
            Duration::from_secs(5)
        );
        assert_eq!(
            ProbeOptions::from_connect_timeout_s(0).connect_timeout,
            Duration::from_secs(1)
        );
        assert_eq!(
            ProbeOptions::from_connect_timeout_s(99).connect_timeout,
            Duration::from_secs(10)
        );
        // 总超时固定 10 秒，不随连接超时变化。
        assert_eq!(
            ProbeOptions::from_connect_timeout_s(2).request_timeout,
            Duration::from_secs(10)
        );
    }

    #[test]
    fn 延迟为_0_时至少记_1ms() {
        assert_eq!(ProbeOutcome::ok(0).latency_ms, Some(1));
    }

    #[test]
    fn ip_直接使用而域名走_dns() {
        assert_eq!(
            resolve_host("104.16.1.1").unwrap(),
            "104.16.1.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            resolve_host("2606:4700::1111").unwrap(),
            "2606:4700::1111".parse::<IpAddr>().unwrap()
        );
        assert!(resolve_host("  ").is_err());
        // localhost 一定能在本机解析出来。
        assert!(resolve_host("localhost").is_ok());
    }

    #[test]
    fn 单条测试落库一条轮次与一条结果() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let prober = fake::FakeProber::always_ok(120);
        let endpoint = Endpoint::new("104.16.1.1", 8443);

        let outcome = run_single(
            &prober,
            &store,
            "run-1",
            "2026-11-13T00:00:00.000Z",
            SingleProbe {
                endpoint: &endpoint,
                alias: "测试别名",
                options: ProbeOptions::default(),
            },
        )
        .unwrap();

        assert_eq!(outcome.status, ProbeStatus::Ok);
        assert_eq!(store.count_test_runs().unwrap(), 1);
        assert_eq!(store.count_test_results().unwrap(), 1);

        let rows = store.list_test_results("run-1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host, "104.16.1.1");
        assert_eq!(rows[0].port, 8443);
        assert_eq!(rows[0].alias, "测试别名");
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].latency_ms, Some(120));
        // 假探测器只被调用一次，说明单条测试不重复采样。
        assert_eq!(prober.calls(), vec![("104.16.1.1".to_string(), 8443)]);
    }

    #[test]
    fn 失败的测试也落库且不计成功() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let prober = fake::FakeProber::new(vec![Attempt::connect_failed("连接被拒绝")]);
        let endpoint = Endpoint::new("104.16.2.2", 2053);

        let outcome = run_single(
            &prober,
            &store,
            "run-2",
            "2026-11-13T00:00:01.000Z",
            SingleProbe {
                endpoint: &endpoint,
                alias: "",
                options: ProbeOptions::default(),
            },
        )
        .unwrap();

        assert_eq!(outcome.status, ProbeStatus::Fail);
        let rows = store.list_test_results("run-2").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "fail");
        assert_eq!(rows[0].latency_ms, None);
        assert_eq!(rows[0].error.as_deref(), Some("连接被拒绝"));
        assert_eq!(store.count_test_results().unwrap(), 1);
    }
}
