//! 单条真连接延迟：为一条优选地址开临时 sing-box 链路并测一次往返。
//!
//! 设计要点（见 ADR-0001 与 ADR-0004）：
//! - sing-box 作为 sidecar 随包内置，按需启动，测完即销毁进程与临时配置；
//! - 探测请求只经本地 mixed 入站出去，不读取系统代理；
//! - 连接阶段未建立记「失败」，连上但总超时内没有响应记「超时」。

pub mod config;
pub mod fake;
pub mod port;
pub mod singbox;

pub use singbox::SingBoxProber;

use std::time::Duration;

use serde::Serialize;

use crate::template::NodeTemplate;

/// 连接超时允许的秒数范围。
pub const MIN_CONNECT_TIMEOUT_S: u32 = 1;
pub const MAX_CONNECT_TIMEOUT_S: u32 = 10;

/// 单次请求的总超时，固定 10 秒，不随设置变化。
pub const REQUEST_TIMEOUT_S: u64 = 10;

/// 探测用的 204 端点。
pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStatus {
    Ok,
    Timeout,
    Fail,
}

impl ProbeStatus {
    /// 落库与日志用的标识。
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::Timeout => "timeout",
            ProbeStatus::Fail => "fail",
        }
    }
}

/// 一条优选地址的测试结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
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
}

/// 一次探测尝试的原始观测，分类只依赖这三个事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// 连接阶段是否已建立（本地入站能连上且出站握手完成）。
    pub connected: bool,
    /// 收到 204 响应的往返毫秒数；`None` 表示没有收到。
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
    let message = attempt
        .error
        .unwrap_or_else(|| "未知错误".to_string());
    if attempt.connected {
        ProbeOutcome::timeout(message)
    } else {
        ProbeOutcome::fail(message)
    }
}

/// 一次探测的参数。连接超时来自设置（1–10 秒），总超时固定 10 秒。#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// 三个动作的统一接口。本期只实现测延迟，其余两个留空待阶段 8 / 9。
pub trait Prober: Send + Sync {
    /// 测一次真连接延迟。
    fn measure_latency(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> ProbeOutcome;

    /// 取出口 IP；阶段 9 实现。
    #[allow(dead_code)] // 阶段 9 接入
    fn exit_ip(
        &self,
        _endpoint: &Endpoint,
        _template: &NodeTemplate,
        _options: &ProbeOptions,
    ) -> Result<String, String> {
        Err("出口 IP 查询将在阶段 9 实现".to_string())
    }

    /// 测速；阶段 8 实现。
    #[allow(dead_code)] // 阶段 8 接入
    fn measure_speed(
        &self,
        _endpoint: &Endpoint,
        _template: &NodeTemplate,
        _options: &ProbeOptions,
    ) -> Result<f64, String> {
        Err("测速将在阶段 8 实现".to_string())
    }
}

/// 一条单条测试的输入。
pub struct SingleProbe<'a> {
    pub endpoint: &'a Endpoint,
    pub template: &'a NodeTemplate,
    pub alias: &'a str,
    pub options: ProbeOptions,
}

/// 跑一条地址的测试并落库：一条测试轮次 + 一条结果。
///
/// 阶段 6 只写单条；阶段 7 的整轮并发会复用同一套落库结构。
#[cfg_attr(not(test), allow(dead_code))] // 命令层直接调 measure + persist_single
pub fn run_single(
    prober: &dyn Prober,
    store: &crate::store::Store,
    run_id: &str,
    started_at: &str,
    probe: SingleProbe<'_>,
) -> Result<ProbeOutcome, String> {
    let outcome = prober.measure_latency(probe.endpoint, probe.template, &probe.options);
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

    // 参数快照只记协议与超时，凭据绝不入库。
    let params = serde_json::json!({
        "kind": probe.template.kind,
        "transport": probe.template.transport,
        "tls": probe.template.tls,
        "connectTimeoutS": probe.options.connect_timeout.as_secs(),
        "requestTimeoutS": probe.options.request_timeout.as_secs(),
        "samples": 1,
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

    fn single<'a>(endpoint: &'a Endpoint, template: &'a NodeTemplate) -> SingleProbe<'a> {
        SingleProbe {
            endpoint,
            template,
            alias: "测试别名",
            options: ProbeOptions::default(),
        }
    }

    #[test]
    fn 单条测试落库一条轮次与一条结果() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let prober = fake::FakeProber::always_ok(120);
        let template = NodeTemplate::default();
        let endpoint = Endpoint::new("104.16.1.1", 8443);

        let outcome = run_single(
            &prober,
            &store,
            "run-1",
            "2026-11-13T00:00:00.000Z",
            single(&endpoint, &template),
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
        let template = NodeTemplate::default();
        let endpoint = Endpoint::new("104.16.2.2", 2053);

        let outcome = run_single(
            &prober,
            &store,
            "run-2",
            "2026-11-13T00:00:01.000Z",
            single(&endpoint, &template),
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
