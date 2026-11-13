//! sing-box 槽位池：并发上限就是槽位数，每槽只启动一次进程，整轮复用。
//!
//! 每个槽位进程里为分到的每条地址准备一条 lane（mixed 入站 + 对应模板的出站），
//! 用路由规则把入站流量固定送到自己的出站；整轮结束后统一杀进程、删临时配置。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::logs::SingboxLog;
use crate::template::NodeTemplate;

use super::batch::{BatchItem, BatchProber};
use super::config::build_slot_config;
use super::port::pick_free_port_avoiding;
use super::singbox::{spawn_singbox, wait_until_ready, TempConfig, STARTUP_TIMEOUT};
use super::speed::{self, SpeedOptions, SpeedProber};
use super::{classify, Attempt, Endpoint, ProbeOptions, ProbeOutcome, PROBE_URL};

/// 一条地址在某个槽位进程里的入口。
struct Lane {
    slot: usize,
    port: u16,
}

/// 一个槽位：一个 sing-box 进程 + 一份临时配置。
struct Slot {
    child: Mutex<Child>,
    _config: TempConfig,
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 整轮批量测试复用的 sing-box 进程池。
pub struct SlotPool {
    slots: Vec<Slot>,
    lanes: HashMap<(String, u16), Lane>,
    log_path: PathBuf,
}

impl SlotPool {
    /// 按并发上限把地址分到各槽位，一次起好全部进程。
    pub fn start(
        binary: impl Into<PathBuf>,
        log: Arc<SingboxLog>,
        log_path: impl Into<PathBuf>,
        temp_dir: impl Into<PathBuf>,
        items: &[BatchItem],
        template: &NodeTemplate,
        concurrency: usize,
    ) -> Result<Self, String> {
        let binary = binary.into();
        let log_path = log_path.into();
        let temp_dir = temp_dir.into();
        let log_hint = || format!("sing-box 日志：{}", log_path.display());

        if items.is_empty() {
            return Ok(Self {
                slots: Vec::new(),
                lanes: HashMap::new(),
                log_path,
            });
        }
        if !binary.is_file() {
            return Err(format!(
                "未找到 sing-box（{}），请先运行 scripts/fetch-singbox.ps1。{}",
                binary.display(),
                log_hint()
            ));
        }

        let concurrency = concurrency.max(1).min(items.len());
        // 轮转分配，槽位之间地址数量尽量平均。
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); concurrency];
        for (index, _) in items.iter().enumerate() {
            buckets[index % concurrency].push(index);
        }

        let mut lanes: HashMap<(String, u16), Lane> = HashMap::new();
        let mut used_ports: HashSet<u16> = HashSet::new();
        let mut slots: Vec<Slot> = Vec::with_capacity(concurrency);
        for (slot_index, bucket) in buckets.iter().enumerate() {
            let mut plan: Vec<(Endpoint, u16)> = Vec::with_capacity(bucket.len());
            for &item_index in bucket {
                let endpoint = &items[item_index].endpoint;
                let Some(port) = pick_free_port_avoiding(&used_ports) else {
                    return Err(format!(
                        "20000–29999 内没有可用端口。{}",
                        log_hint()
                    ));
                };
                used_ports.insert(port);
                lanes.insert(
                    (endpoint.host.clone(), endpoint.port),
                    Lane {
                        slot: slot_index,
                        port,
                    },
                );
                plan.push((endpoint.clone(), port));
            }

            let config = build_slot_config(&plan, template);
            let contents = serde_json::to_string_pretty(&config).unwrap_or_default();
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let name = format!("slot-{}-{slot_index}-{unique}.json", std::process::id());
            let config = TempConfig::create(&temp_dir, &name, &contents)
                .map_err(|error| format!("写入临时配置失败：{error}。{}", log_hint()))?;

            let mut child = spawn_singbox(&binary, config.path(), &log)
                .map_err(|error| format!("启动 sing-box 失败：{error}。{}", log_hint()))?;

            for (_, port) in &plan {
                if let Err(error) = wait_until_ready(&mut child, *port, STARTUP_TIMEOUT) {
                    // child 与 config 在这里被回收，已建好的槽位由 Vec 的 Drop 回收。
                    return Err(format!("{error}。{}", log_hint()));
                }
            }

            slots.push(Slot {
                child: Mutex::new(child),
                _config: config,
            });
        }

        Ok(Self {
            slots,
            lanes,
            log_path,
        })
    }

    /// 槽位数量，测试与日志用。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// 经该地址所属槽位的本地 mixed 入站测一次。
    pub fn measure(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome {
        let Some(lane) = self.lanes.get(&(endpoint.host.clone(), endpoint.port)) else {
            return ProbeOutcome::fail("槽位池里没有这条地址");
        };

        let client = match reqwest::blocking::Client::builder()
            .proxy(match reqwest::Proxy::all(format!("http://127.0.0.1:{}", lane.port)) {
                Ok(proxy) => proxy,
                Err(error) => return ProbeOutcome::fail(format!("本地代理地址无效：{error}")),
            })
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .build()
        {
            Ok(client) => client,
            Err(error) => return ProbeOutcome::fail(format!("创建探测客户端失败：{error}")),
        };

        let started = Instant::now();
        let result = client.get(PROBE_URL).send();
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(response) if response.status() == reqwest::StatusCode::NO_CONTENT => {
                classify(Attempt::responded(elapsed_ms))
            }
            Ok(response) => classify(Attempt::connect_failed(format!(
                "204 端点返回 HTTP {}",
                response.status().as_u16()
            ))),
            Err(error) => {
                // 槽位进程中途退出：立即失败并给出日志路径。
                if let Ok(mut child) = self.slots[lane.slot].child.lock() {
                    if let Ok(Some(status)) = child.try_wait() {
                        return ProbeOutcome::fail(format!(
                            "sing-box 槽位中途退出（{status}），本轮已终止。sing-box 日志：{}",
                            self.log_path.display()
                        ));
                    }
                }
                if error.is_timeout() {
                    classify(Attempt::no_response(format!(
                        "{} 秒内未收到响应（{}）",
                        options.request_timeout.as_secs(),
                        error
                    )))
                } else {
                    classify(Attempt::connect_failed(format!("连接阶段失败：{error}")))
                }
            }
        }
    }
}

impl BatchProber for SlotPool {
    fn sample(
        &self,
        endpoint: &Endpoint,
        _template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> ProbeOutcome {
        self.measure(endpoint, options)
    }
}

impl SpeedProber for SlotPool {
    /// 经该地址所属槽位的本地 mixed 入站并发下载，换算成 MB/s。
    fn speed_sample(
        &self,
        endpoint: &Endpoint,
        options: &ProbeOptions,
        speed: &SpeedOptions,
        cancel: &AtomicBool,
    ) -> Result<f64, String> {
        let Some(lane) = self.lanes.get(&(endpoint.host.clone(), endpoint.port)) else {
            return Err("槽位池里没有这条地址".to_string());
        };

        let result = speed::download_streams(
            Some(&format!("http://127.0.0.1:{}", lane.port)),
            options.connect_timeout,
            speed,
            cancel,
        );

        // 槽位进程中途退出时，把日志路径带进失败原因。
        if result.is_err() {
            if let Ok(mut child) = self.slots[lane.slot].child.lock() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!(
                        "sing-box 槽位中途退出（{status}），本轮已终止。sing-box 日志：{}",
                        self.log_path.display()
                    ));
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空地址池不要求_binary_也不启动进程() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(SingboxLog::spawn(Arc::new(crate::logs::LogWriter::new(
            dir.path(),
        ))));
        let pool = SlotPool::start(
            dir.path().join("nope.exe"),
            log.clone(),
            dir.path().join("singbox.log"),
            dir.path().join("tmp"),
            &[],
            &NodeTemplate::default(),
            4,
        )
        .unwrap();

        assert_eq!(pool.slot_count(), 0);
        assert_eq!(
            pool.measure(
                &Endpoint::new("104.16.1.1", 8443),
                &ProbeOptions::default()
            )
            .status,
            super::super::ProbeStatus::Fail
        );
        log.shutdown();
    }

    #[test]
    fn 二进制不存在时返回带日志路径的错误() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(SingboxLog::spawn(Arc::new(crate::logs::LogWriter::new(
            dir.path(),
        ))));
        let items = vec![BatchItem::new(Endpoint::new("104.16.1.1", 8443), "")];
        let log_path = dir.path().join("singbox.log");
        let error = match SlotPool::start(
            dir.path().join("nope.exe"),
            log.clone(),
            log_path.clone(),
            dir.path().join("tmp"),
            &items,
            &NodeTemplate::default(),
            2,
        ) {
            Ok(_) => panic!("二进制不存在时应返回错误"),
            Err(error) => error,
        };

        assert!(error.contains(&log_path.display().to_string()));
        log.shutdown();
    }
}
