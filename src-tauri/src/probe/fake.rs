//! 测试用的假探测器：不发起任何网络请求，按预设返回结论。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::batch::BatchProber;
use super::speed::{SpeedOptions, SpeedProber};
use super::{Attempt, Endpoint, HostResolver, ProbeOptions, ProbeOutcome, Prober};

/// 按预设尝试结果返回，并记录调用过的地址。
#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
#[derive(Default)]
pub struct FakeProber {
    /// 每次 `measure_latency` 依次取一个；用尽后重复最后一个。
    attempts: Mutex<Vec<Attempt>>,
    /// 按地址预设的采样序列，优先于 `attempts`；批量测试用。
    scripted: Mutex<HashMap<(String, u16), Vec<Attempt>>>,
    /// 按地址预设的测速结果。
    speeds: Mutex<HashMap<(String, u16), Result<f64, String>>>,
    /// 按地址预设的解析结果。
    ips: Mutex<HashMap<(String, u16), Result<IpAddr, String>>>,
    calls: Mutex<Vec<(String, u16)>>,
    speed_calls: Mutex<Vec<(String, u16)>>,
    /// 每次采样模拟的耗时，用来让并发真正重叠。
    delay: Duration,
    /// 当前正在采样的线程数，用来断言并发峰值。
    in_flight: AtomicUsize,
    /// 观察到的并发峰值。
    peak_in_flight: AtomicUsize,
}

#[cfg_attr(not(test), allow(dead_code))] // 只在测试里构造
impl FakeProber {
    pub fn new(attempts: Vec<Attempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts),
            ..Self::default()
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
            scripted: Mutex::new(scripted),
            ..Self::default()
        }
    }

    /// 预设每条地址的测速结果；没预设的地址返回失败。
    pub fn with_speeds(
        self,
        entries: Vec<((&str, u16), Result<f64, String>)>,
    ) -> Self {
        if let Ok(mut speeds) = self.speeds.lock() {
            for ((host, port), result) in entries {
                speeds.insert((host.to_string(), port), result);
            }
        }
        self
    }

    /// 预设每条地址的解析结果；没预设的地址按 IP 直接解析。
    pub fn with_ips(self, entries: Vec<((&str, u16), Result<IpAddr, String>)>) -> Self {
        if let Ok(mut ips) = self.ips.lock() {
            for ((host, port), result) in entries {
                ips.insert((host.to_string(), port), result);
            }
        }
        self
    }

    pub fn calls(&self) -> Vec<(String, u16)> {
        self.calls.lock().map(|calls| calls.clone()).unwrap_or_default()
    }

    /// 让每次采样先睡一会儿，制造可以重叠的并发窗口。
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// 观察到的并发峰值（同时在采样中的线程数）。
    pub fn peak_concurrency(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    pub fn speed_calls(&self) -> Vec<(String, u16)> {
        self.speed_calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
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
    fn measure_latency(&self, endpoint: &Endpoint, _options: &ProbeOptions) -> ProbeOutcome {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((endpoint.host.clone(), endpoint.port));
        }
        let attempt = self.next_attempt(endpoint);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        super::classify(attempt)
    }
}

impl BatchProber for FakeProber {
    fn sample(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome {
        self.measure_latency(endpoint, options)
    }
}

impl HostResolver for FakeProber {
    fn resolve(&self, endpoint: &Endpoint) -> Result<IpAddr, String> {
        if let Some(result) = self
            .ips
            .lock()
            .ok()
            .and_then(|ips| ips.get(&(endpoint.host.clone(), endpoint.port)).cloned())
        {
            return result;
        }
        super::resolve_host(&endpoint.host)
    }
}

impl SpeedProber for FakeProber {
    fn speed_sample(
        &self,
        endpoint: &Endpoint,
        _options: &ProbeOptions,
        _speed: &SpeedOptions,
        _cancel: &AtomicBool,
    ) -> Result<f64, String> {
        if let Ok(mut calls) = self.speed_calls.lock() {
            calls.push((endpoint.host.clone(), endpoint.port));
        }
        self.speeds
            .lock()
            .ok()
            .and_then(|speeds| speeds.get(&(endpoint.host.clone(), endpoint.port)).cloned())
            .unwrap_or_else(|| Err("假探测器没有预设测速结果".to_string()))
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
        let options = ProbeOptions::default();

        assert_eq!(
            prober.measure_latency(&endpoint(), &options).status,
            ProbeStatus::Ok
        );
        assert_eq!(
            prober.measure_latency(&endpoint(), &options).status,
            ProbeStatus::Timeout
        );
        assert_eq!(
            prober.measure_latency(&endpoint(), &options).status,
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
        let options = ProbeOptions::default();

        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &options)
                .latency_ms,
            Some(90)
        );
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &options)
                .latency_ms,
            Some(70)
        );
        // 序列用尽后重复最后一次。
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.1", 8443), &options)
                .latency_ms,
            Some(70)
        );
        assert_eq!(
            prober
                .measure_latency(&Endpoint::new("104.16.1.2", 8443), &options)
                .status,
            ProbeStatus::Fail
        );
    }

    #[test]
    fn 解析结果可以按地址预设() {
        let prober = FakeProber::default().with_ips(vec![(
            ("cdn.example.com", 443),
            Ok("203.0.113.7".parse().unwrap()),
        )]);
        assert_eq!(
            prober
                .resolve(&Endpoint::new("cdn.example.com", 443))
                .unwrap()
                .to_string(),
            "203.0.113.7"
        );
        // 没预设时按 IP 直接解析。
        assert_eq!(
            prober
                .resolve(&Endpoint::new("104.16.1.1", 8443))
                .unwrap()
                .to_string(),
            "104.16.1.1"
        );
    }
}
