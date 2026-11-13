//! 测试用的假探测器：不启动任何进程，按预设返回结论。

use std::sync::Mutex;

use crate::template::NodeTemplate;

use super::{Attempt, Endpoint, ProbeOptions, ProbeOutcome, Prober};

/// 按预设尝试结果返回，并记录调用过的地址。
#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
#[derive(Default)]
pub struct FakeProber {
    /// 每次 `measure_latency` 依次取一个；用尽后重复最后一个。
    attempts: Mutex<Vec<Attempt>>,
    calls: Mutex<Vec<(String, u16)>>,
}

#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
impl FakeProber {
    pub fn new(attempts: Vec<Attempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// 永远成功的假探测器。
    pub fn always_ok(latency_ms: u64) -> Self {
        Self::new(vec![Attempt::responded(latency_ms)])
    }

    pub fn calls(&self) -> Vec<(String, u16)> {
        self.calls.lock().map(|calls| calls.clone()).unwrap_or_default()
    }
}

impl Prober for FakeProber {
    fn measure_latency(
        &self,
        endpoint: &Endpoint,
        _template: &NodeTemplate,
        _options: &ProbeOptions,
    ) -> ProbeOutcome {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((endpoint.host.clone(), endpoint.port));
        }
        let attempt = {
            let mut attempts = self.attempts.lock().expect("假探测器的尝试列表被毒化");
            if attempts.is_empty() {
                Attempt::connect_failed("假探测器没有预设结果")
            } else if attempts.len() == 1 {
                attempts[0].clone()
            } else {
                attempts.remove(0)
            }
        };
        super::classify(attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ProbeStatus;

    fn endpoint() -> Endpoint {
        Endpoint::new("104.16.1.1", 8443)
    }

    #[test]
    fn 假探测器按顺序返回预设结果() {
        let prober = FakeProber::new(vec![
            Attempt::responded(88),
            Attempt::no_response("超时"),
            Attempt::connect_failed("拒绝"),
        ]);
        let template = NodeTemplate::default();
        let options = ProbeOptions::default();

        assert_eq!(
            prober.measure_latency(&endpoint(), &template, &options).status,
            ProbeStatus::Ok
        );
        assert_eq!(
            prober.measure_latency(&endpoint(), &template, &options).status,
            ProbeStatus::Timeout
        );
        assert_eq!(
            prober.measure_latency(&endpoint(), &template, &options).status,
            ProbeStatus::Fail
        );
        assert_eq!(prober.calls().len(), 3);
    }
}
