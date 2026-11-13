//! 测试用的假探测器：不启动任何进程，按预设返回结论。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::template::NodeTemplate;

use super::batch::{BatchItem, BatchProber};
use super::{Attempt, Endpoint, ProbeOptions, ProbeOutcome, Prober};

/// 按预设尝试结果返回，并记录调用过的地址。
#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
#[derive(Default)]
pub struct FakeProber {
    /// 每次 `measure_latency` 依次取一个；用尽后重复最后一个。
    attempts: Mutex<Vec<Attempt>>,
    /// 按地址预设的采样序列，优先于 `attempts`；批量测试用。
    scripted: Mutex<HashMap<(String, u16), Vec<Attempt>>>,
    calls: Mutex<Vec<(String, u16)>>,
}

#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
impl FakeProber {
    pub fn new(attempts: Vec<Attempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts),
            scripted: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// 永远成功的假探测器。
    pub fn always_ok(latency_ms: u64) -> Self {
        Self::new(vec![Attempt::responded(latency_ms)])
    }

    /// 按地址预设每次采样的原始观测；采样用尽后重复最后一次。
    pub fn scripted(entries: Vec<((&str, u16), Vec<Attempt>)>) -> Self {
        let scripted = entries
            .into_iter()
            .map(|((host, port), attempts)| ((host.to_string(), port), attempts))
            .collect();
        Self {
            attempts: Mutex::new(Vec::new()),
            scripted: Mutex::new(scripted),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<(String, u16)> {
        self.calls.lock().map(|calls| calls.clone()).unwrap_or_default()
    }

    fn next_attempt(&self, endpoint: &Endpoint) -> Attempt {
        if let Ok(mut scripted) = self.scripted.lock() {
            if let Some(list) = scripted.get_mut(&(endpoint.host.clone(), endpoint.port)) {
                if list.len() > 1 {
                    return list.remove(0);
                }
                if let Some(last) = list.first() {
                    return last.clone();
                }
            }
        }
        let mut attempts = self.attempts.lock().expect("假探测器的尝试列表被毒化");
        if attempts.is_empty() {
            Attempt::connect_failed("假探测器没有预设结果")
        } else if attempts.len() == 1 {
            attempts[0].clone()
        } else {
            attempts.remove(0)
        }
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
        super::classify(self.next_attempt(endpoint))
    }
}

impl BatchProber for FakeProber {
    fn prepare(
        &self,
        _items: &[BatchItem],
        _template: &NodeTemplate,
        _concurrency: usize,
    ) -> Result<(), String> {
        Ok(())
    }

    fn sample(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> ProbeOutcome {
        self.measure_latency(endpoint, template, options)
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

    #[test]
    fn 按地址预设的采样互不干扰() {
        let prober = FakeProber::scripted(vec![
            (
                ("104.16.1.1", 8443),
                vec![Attempt::responded(90), Attempt::responded(70)],
            ),
            (
                ("104.16.1.2", 8443),
                vec![Attempt::connect_failed("拒绝")],
            ),
        ]);
        let template = NodeTemplate::default();
        let options = ProbeOptions::default();

        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &template, &options)
                .latency_ms,
            Some(90)
        );
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &template, &options)
                .latency_ms,
            Some(70)
        );
        // 序列用尽后重复最后一次。
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &template, &options)
                .latency_ms,
            Some(70)
        );
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.2", 8443), &template, &options)
                .status,
            ProbeStatus::Fail
        );
    }
}
