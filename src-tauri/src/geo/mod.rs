//! 出口 IP 与归属地（阶段 9）。
//!
//! 两步走（见 ADR-0002）：
//! 1. 经当前代理链路请求两个端点，交叉校验拿到**出口 IP**——不是优选地址本身；
//! 2. 对出口 IP 查归属地：内置离线库优先，`ipwho.is` 只作兜底。
//!
//! 中英文名统一走 `countries` 的本地表，在线源的中文译名一律不用，避免繁简混排。
//! Cloudflare 落地只能给机房三字码，界面显示 `Cloudflare / Cloudflare / LAX`，
//! 不硬编城市名。

pub mod countries;
pub mod exit;
pub mod offline;
pub mod online;

pub use countries::country_name;
pub use exit::fetch_exit_ip;
pub use offline::OfflineDb;
pub use online::IpWhoIs;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::probe::batch::{BatchItem, BatchProgress};
use crate::probe::{Endpoint, ProbeOptions};
use crate::store::Store;

/// Cloudflare 的 ASN，用于判定「出口仍在 CF 网内」。
pub const CLOUDFLARE_ASN: u32 = 13335;
/// Cloudflare 落地时归属地两段显示的文字。
pub const CLOUDFLARE_NAME: &str = "Cloudflare";

/// 已知的 Cloudflare IPv4 网段（公开的 anycast 段）。
const CLOUDFLARE_V4: &[(&str, u8)] = &[
    ("173.245.48.0", 20),
    ("103.21.244.0", 22),
    ("103.22.200.0", 22),
    ("103.31.4.0", 22),
    ("141.101.64.0", 18),
    ("108.162.192.0", 18),
    ("190.93.240.0", 20),
    ("188.114.96.0", 20),
    ("197.234.240.0", 22),
    ("198.41.128.0", 17),
    ("162.158.0.0", 15),
    ("104.16.0.0", 13),
    ("104.24.0.0", 14),
    ("172.64.0.0", 13),
    ("131.0.72.0", 22),
];

/// 已知的 Cloudflare IPv6 网段。
const CLOUDFLARE_V6: &[(&str, u8)] = &[
    ("2400:cb00::", 32),
    ("2606:4700::", 32),
    ("2803:f800::", 32),
    ("2405:b500::", 32),
    ("2405:8100::", 32),
    ("2a06:98c0::", 29),
    ("2c0f:f248::", 32),
];

/// 一次探测拿到的出口信息。`asn` 由归属地查询补齐。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitIp {
    pub ip: String,
    /// `cdn-cgi/trace` 的 `loc`，两位国家码。
    pub loc: Option<String>,
    /// `cdn-cgi/trace` 的 `colo`，机房三字码。
    pub colo: Option<String>,
    /// 出口 IP 的 ASN；离线库给不出，在线查询才可能有。
    pub asn: Option<u32>,
}

impl ExitIp {
    #[cfg_attr(not(test), allow(dead_code))] // 单条命令与测试构造
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            ..Self::default()
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_trace(mut self, loc: Option<String>, colo: Option<String>) -> Self {
        self.loc = loc.filter(|value| !value.trim().is_empty());
        self.colo = colo.filter(|value| !value.trim().is_empty());
        self
    }
}

/// 单个端点的一次观测，`ip` 缺失表示该端点没给出结论。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitSample {
    pub ip: Option<String>,
    pub loc: Option<String>,
    pub colo: Option<String>,
}

/// 两个端点的观测取多数值：同一 IP 出现次数最多的胜出，平票取先出现的。
///
/// 只有一个端点成功时结论就是它——单点失败不影响结论。
pub fn pick_exit_ip(samples: &[ExitSample]) -> Option<ExitIp> {
    let mut order: Vec<String> = Vec::new();
    for sample in samples {
        if let Some(ip) = sample.ip.as_deref().map(str::trim).filter(|ip| !ip.is_empty()) {
            if !order.iter().any(|known| known == ip) {
                order.push(ip.to_string());
            }
        }
    }

    let mut best: Option<(String, usize)> = None;
    for ip in &order {
        let count = samples
            .iter()
            .filter(|sample| sample.ip.as_deref().map(str::trim) == Some(ip.as_str()))
            .count();
        if best
            .as_ref()
            .map_or(true, |(_, best_count)| count > *best_count)
        {
            best = Some((ip.clone(), count));
        }
    }

    let (ip, _) = best?;
    let matches = |sample: &&ExitSample| sample.ip.as_deref().map(str::trim) == Some(ip.as_str());
    let loc = samples
        .iter()
        .filter(matches)
        .find_map(|sample| sample.loc.clone());
    let colo = samples
        .iter()
        .filter(matches)
        .find_map(|sample| sample.colo.clone());
    Some(ExitIp {
        ip,
        loc,
        colo,
        asn: None,
    })
}

fn ipv4_in(network: &str, bits: u8, address: u32) -> bool {
    let Ok(base) = network.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(bits))
    };
    address & mask == u32::from(base) & mask
}

fn ipv6_in(network: &str, bits: u8, address: u128) -> bool {
    let Ok(base) = network.parse::<std::net::Ipv6Addr>() else {
        return false;
    };
    let mask = if bits == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(bits))
    };
    address & mask == u128::from(base) & mask
}

/// 出口是否仍在 Cloudflare 网内：ASN 是 13335，或 IP 落在已知 CF 段。
///
/// 只有这种情况才显示机房三字码；普通落地即使 trace 里有 `colo`，
/// 那也只是请求进入 CF 的 POP，不代表落地位置，所以不显示（见 GLOSSARY「机房」）。
pub fn is_cloudflare_ip(ip: &str, asn: Option<u32>) -> bool {
    if asn == Some(CLOUDFLARE_ASN) {
        return true;
    }
    let Ok(address) = ip.trim().parse::<std::net::IpAddr>() else {
        return false;
    };
    match address {
        std::net::IpAddr::V4(v4) => CLOUDFLARE_V4
            .iter()
            .any(|(network, bits)| ipv4_in(network, *bits, u32::from(v4))),
        std::net::IpAddr::V6(v6) => CLOUDFLARE_V6
            .iter()
            .any(|(network, bits)| ipv6_in(network, *bits, u128::from(v6))),
    }
}

/// 一条归属地记录（缓存与落库的最小单位）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoRecord {
    /// ISO 国家码；Cloudflare 落地为空。
    pub country: String,
    pub zh: String,
    pub en: String,
    pub asn: Option<u32>,
}

impl GeoRecord {
    /// 按国家码构造，中英文名走本地表。
    pub fn from_country(code: &str, asn: Option<u32>) -> Self {
        let (zh, en) = country_name(code);
        Self {
            country: code.trim().to_uppercase(),
            zh,
            en,
            asn,
        }
    }
}

/// 一条地址的归属地结论，界面三段显示就用它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoInfo {
    pub exit_ip: String,
    pub country: String,
    pub zh: String,
    pub en: String,
    /// 机房三字码；非 Cloudflare 落地或没有 colo 时为 `None`。
    pub colo: Option<String>,
    pub asn: Option<u32>,
    /// 数据来源：`offline` / `online` / `cache` / `trace`。
    pub source: String,
}

impl GeoInfo {
    /// 「中文 / English / 机房」三段；无机房时第三段是空串。
    #[cfg_attr(not(test), allow(dead_code))] // 导出（阶段 10）与测试用
    pub fn three_parts(&self) -> (String, String, String) {
        (
            self.zh.clone(),
            self.en.clone(),
            self.colo.clone().unwrap_or_default(),
        )
    }

    /// 一段式展示，导出与日志用。
    #[allow(dead_code)] // 阶段 10 导出直接复用
    pub fn display(&self) -> String {
        let (zh, en, colo) = self.three_parts();
        format!("{zh} / {en} / {colo}")
    }

    /// 是否是 Cloudflare 落地。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_cloudflare(&self) -> bool {
        self.zh == CLOUDFLARE_NAME
    }
}

/// 离线库接口：只给国家码（`asn` 恒为 `None`）。
pub trait CountryDb: Send + Sync {
    fn lookup(&self, ip: &str) -> Result<GeoRecord, String>;
}

/// 在线兜底接口。
pub trait OnlineGeo: Send + Sync {
    fn lookup(&self, ip: &str) -> Result<GeoRecord, String>;
}

/// 归属地查询器：内存缓存 + 离线优先 + 在线兜底。
pub struct GeoResolver {
    offline: Box<dyn CountryDb>,
    online: Box<dyn OnlineGeo>,
    cache: Mutex<HashMap<String, GeoRecord>>,
}

impl GeoResolver {
    /// 生产用：内置离线库 + `ipwho.is`。
    pub fn new(offline_candidates: Vec<std::path::PathBuf>) -> Self {
        Self::with_lookups(
            Box::new(OfflineDb::new(offline_candidates)),
            Box::new(IpWhoIs::new()),
        )
    }

    /// 测试用：注入两个数据源。
    pub fn with_lookups(offline: Box<dyn CountryDb>, online: Box<dyn OnlineGeo>) -> Self {
        Self {
            offline,
            online,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 把落库的缓存灌进内存，避免同一 IP 跨轮次重复查询。
    pub fn preload<I>(&self, entries: I)
    where
        I: IntoIterator<Item = (String, GeoRecord)>,
    {
        if let Ok(mut cache) = self.cache.lock() {
            for (ip, record) in entries {
                cache.insert(ip.trim().to_string(), record);
            }
        }
    }

    pub fn cached(&self, ip: &str) -> Option<GeoRecord> {
        self.cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(ip.trim()).cloned())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cache_len(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }

    fn remember(&self, ip: &str, record: &GeoRecord) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(ip.trim().to_string(), record.clone());
        }
    }

    /// 查一条出口 IP 的归属地。
    ///
    /// 顺序：内存缓存 → 离线库 → 在线兜底 → trace 的 `loc`。
    /// 全部失败才返回错误；调用方据此只留空归属地三列，不动延迟与速度。
    pub fn locate(&self, exit: &ExitIp) -> Result<GeoInfo, String> {
        let ip = exit.ip.trim();
        if ip.is_empty() {
            return Err("出口 IP 为空，无法查归属地".to_string());
        }

        let (record, source) = match self.cached(ip) {
            Some(record) => (record, "cache"),
            None => {
                let (record, source) = match self.offline.lookup(ip) {
                    Ok(record) => (record, "offline"),
                    Err(offline_error) => match self.online.lookup(ip) {
                        Ok(record) => (record, "online"),
                        Err(online_error) => match exit.loc.as_deref() {
                            Some(loc) if !loc.trim().is_empty() => {
                                (GeoRecord::from_country(loc, None), "trace")
                            }
                            _ => {
                                return Err(format!(
                                    "归属地查询失败：{offline_error}；{online_error}"
                                ))
                            }
                        },
                    },
                };
                let record = cloudflare_record(record, ip);
                self.remember(ip, &record);
                (record, source)
            }
        };

        Ok(self.info(exit, record, source))
    }

    fn info(&self, exit: &ExitIp, record: GeoRecord, source: &str) -> GeoInfo {
        // Cloudflare 落地只给机房三字码；普通落地的 colo 只是入口 POP，不显示。
        let cloudflare = is_cloudflare_ip(&exit.ip, record.asn) || record.zh == CLOUDFLARE_NAME;
        let colo = if cloudflare {
            exit.colo
                .as_deref()
                .map(|colo| colo.trim().to_uppercase())
                .filter(|colo| !colo.is_empty())
        } else {
            None
        };
        GeoInfo {
            exit_ip: exit.ip.trim().to_string(),
            country: record.country,
            zh: record.zh,
            en: record.en,
            colo,
            asn: record.asn,
            source: source.to_string(),
        }
    }
}

/// 出口仍在 CF 网内时，归属地两段统一显示 `Cloudflare`，不硬编城市名。
fn cloudflare_record(record: GeoRecord, ip: &str) -> GeoRecord {
    if !is_cloudflare_ip(ip, record.asn) {
        return record;
    }
    GeoRecord {
        country: String::new(),
        zh: CLOUDFLARE_NAME.to_string(),
        en: CLOUDFLARE_NAME.to_string(),
        asn: record.asn.or(Some(CLOUDFLARE_ASN)),
    }
}

/// 能取出口 IP 的探测器：槽位池与假探测器都实现它。
pub trait GeoProber: Send + Sync {
    fn exit_ip_sample(
        &self,
        endpoint: &Endpoint,
        options: &ProbeOptions,
        cancel: &AtomicBool,
    ) -> Result<ExitIp, String>;
}

/// 一条地址的归属地结果。
#[derive(Debug, Clone, PartialEq)]
pub struct GeoRow {
    pub endpoint: Endpoint,
    pub alias: String,
    pub exit_ip: Option<String>,
    pub info: Option<GeoInfo>,
    pub error: Option<String>,
}

/// 一轮归属地查询的统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoCounts {
    pub ok: usize,
    pub fail: usize,
    pub untested: usize,
}

/// 一轮归属地查询的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct GeoReport {
    pub rows: Vec<GeoRow>,
    pub total: usize,
    pub completed: usize,
    pub stopped: bool,
}

impl GeoReport {
    pub fn counts(&self) -> GeoCounts {
        let mut counts = GeoCounts::default();
        for row in &self.rows {
            if row.info.is_some() {
                counts.ok += 1;
            } else if row.error.is_some() {
                counts.fail += 1;
            } else {
                counts.untested += 1;
            }
        }
        counts
    }
}

/// 跑一轮归属地查询：地址间并发沿用设置，失败只影响自己那一行。
///
/// 分两段：先并发取每条的出口 IP，再对**去重后的出口 IP**并发查归属地。
/// 同一 IP 只查一次，既省在线额度，也避免并发重复计费。
pub fn run_geo_batch(
    prober: &dyn GeoProber,
    resolver: &GeoResolver,
    items: &[BatchItem],
    options: &ProbeOptions,
    concurrency: usize,
    cancel: &AtomicBool,
    on_progress: &(dyn Fn(BatchProgress) + Sync),
) -> GeoReport {
    let total = items.len();
    let concurrency = concurrency.clamp(1, 32);

    // 第一段：每条地址各取一次出口 IP。
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let exits: Vec<Mutex<Option<Result<ExitIp, String>>>> =
        (0..total).map(|_| Mutex::new(None)).collect();

    let workers = concurrency.min(total);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= total {
                    break;
                }
                let result = prober.exit_ip_sample(&items[index].endpoint, options, cancel);
                if let Ok(mut slot) = exits[index].lock() {
                    *slot = Some(result);
                }

                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                on_progress(BatchProgress {
                    completed: done,
                    total,
                });
            });
        }
    });

    let exits: Vec<Option<Result<ExitIp, String>>> = exits
        .into_iter()
        .map(|slot| slot.into_inner().unwrap_or(None))
        .collect();

    // 第二段：出口 IP 去重后查询归属地，同一 IP 只查一次。
    // 每个 IP 保留第一次观测到的 loc / colo，用来判断是否显示机房。
    let mut unique: Vec<ExitIp> = Vec::new();
    for exit in exits.iter().flatten() {
        if let Ok(exit) = exit {
            let ip = exit.ip.trim().to_string();
            if !ip.is_empty() && !unique.iter().any(|known| known.ip == ip) {
                unique.push(ExitIp {
                    ip,
                    loc: exit.loc.clone(),
                    colo: exit.colo.clone(),
                    asn: exit.asn,
                });
            }
        }
    }
    let lookups: Vec<Mutex<Option<Result<GeoInfo, String>>>> =
        (0..unique.len()).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let lookup_workers = concurrency.min(unique.len());
    std::thread::scope(|scope| {
        for _ in 0..lookup_workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= unique.len() {
                    break;
                }
                // 缓存命中的 IP 在第二段里是纯内存操作。
                let result = resolver.locate(&unique[index]);
                if let Ok(mut slot) = lookups[index].lock() {
                    *slot = Some(result);
                }
            });
        }
    });
    let lookups: Vec<Option<Result<GeoInfo, String>>> = lookups
        .into_iter()
        .map(|slot| slot.into_inner().unwrap_or(None))
        .collect();
    let lookup_of = |ip: &str| -> Option<Result<GeoInfo, String>> {
        unique
            .iter()
            .position(|known| known.ip == ip)
            .and_then(|index| lookups[index].clone())
    };

    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut row = GeoRow {
                endpoint: item.endpoint.clone(),
                alias: item.alias.clone(),
                exit_ip: None,
                info: None,
                error: None,
            };
            match exits.get(index).cloned().flatten() {
                Some(Ok(exit)) => {
                    row.exit_ip = Some(exit.ip.clone());
                    match lookup_of(exit.ip.trim()) {
                        Some(Ok(info)) => row.info = Some(info),
                        Some(Err(error)) => row.error = Some(error),
                        None => row.error = Some("归属地查询没有结论".to_string()),
                    }
                }
                Some(Err(error)) => row.error = Some(error),
                None => {}
            }
            row
        })
        .collect();

    GeoReport {
        rows,
        total,
        completed: completed.load(Ordering::SeqCst),
        stopped: cancel.load(Ordering::SeqCst),
    }
}

/// 把归属地写进该轮已有的结果行，并更新 IP 缓存。
///
/// 只写成功的那几行：查询失败的行原样保留（不清空延迟、速度与上一次的归属地）。
pub fn persist_geo(store: &Store, run_id: &str, rows: &[GeoRow]) -> Result<(), String> {
    for row in rows {
        let Some(info) = &row.info else {
            continue;
        };
        store
            .upsert_test_result_geo(run_id, &row.endpoint, &row.alias, info)
            .map_err(|e| e.to_string())?;
        store
            .upsert_geo_cache(
                &info.exit_ip,
                &GeoRecord {
                    country: info.country.clone(),
                    zh: info.zh.clone(),
                    en: info.en.clone(),
                    asn: info.asn,
                },
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::probe::fake::FakeProber;
    use crate::probe::ProbeOutcome;

    /// 固定结果的假离线库，记录被查了几次。
    struct FixedCountry {
        record: Option<GeoRecord>,
        calls: AtomicUsize,
    }

    impl FixedCountry {
        fn ok(record: GeoRecord) -> Self {
            Self {
                record: Some(record),
                calls: AtomicUsize::new(0),
            }
        }

        fn missing() -> Self {
            Self {
                record: None,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl CountryDb for FixedCountry {
        fn lookup(&self, _ip: &str) -> Result<GeoRecord, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.record
                .clone()
                .ok_or_else(|| "离线库不可用".to_string())
        }
    }

    struct FixedOnline {
        record: Option<GeoRecord>,
        calls: AtomicUsize,
    }

    impl FixedOnline {
        fn ok(record: GeoRecord) -> Self {
            Self {
                record: Some(record),
                calls: AtomicUsize::new(0),
            }
        }

        fn broken() -> Self {
            Self {
                record: None,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl OnlineGeo for FixedOnline {
        fn lookup(&self, _ip: &str) -> Result<GeoRecord, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.record
                .clone()
                .ok_or_else(|| "在线源不可用".to_string())
        }
    }

    fn canada() -> GeoRecord {
        GeoRecord::from_country("CA", Some(15169))
    }

    #[test]
    fn 双端点多数取值() {
        let samples = vec![
            ExitSample {
                ip: Some("203.0.113.1".into()),
                loc: Some("CA".into()),
                colo: None,
            },
            ExitSample {
                ip: Some("203.0.113.2".into()),
                loc: Some("US".into()),
                colo: Some("LAX".into()),
            },
            ExitSample {
                ip: Some("203.0.113.1".into()),
                loc: Some("CA".into()),
                colo: None,
            },
        ];
        let exit = pick_exit_ip(&samples).unwrap();
        assert_eq!(exit.ip, "203.0.113.1");
        assert_eq!(exit.loc.as_deref(), Some("CA"));
    }

    #[test]
    fn 只有一个端点成功时结论就是它() {
        let exit = pick_exit_ip(&[ExitSample {
            ip: Some("198.51.100.7".into()),
            loc: Some("JP".into()),
            colo: Some("NRT".into()),
        }])
        .unwrap();
        assert_eq!(exit.ip, "198.51.100.7");
        assert_eq!(exit.colo.as_deref(), Some("NRT"));

        // 两个端点都没给出 IP 时没有结论。
        assert_eq!(pick_exit_ip(&[ExitSample::default()]), None);
        assert_eq!(pick_exit_ip(&[]), None);
    }

    #[test]
    fn 两端一致时合并_loc_与_colo() {
        let samples = vec![
            ExitSample {
                ip: Some("203.0.113.5".into()),
                loc: None,
                colo: None,
            },
            ExitSample {
                ip: Some("203.0.113.5".into()),
                loc: Some("DE".into()),
                colo: Some("FRA".into()),
            },
        ];
        let exit = pick_exit_ip(&samples).unwrap();
        assert_eq!(exit.loc.as_deref(), Some("DE"));
        assert_eq!(exit.colo.as_deref(), Some("FRA"));
    }

    #[test]
    fn cf_落地判定按_asn_或网段() {
        assert!(is_cloudflare_ip("104.16.1.1", None));
        assert!(is_cloudflare_ip("162.158.10.1", None));
        assert!(is_cloudflare_ip("2606:4700::1111", None));
        assert!(is_cloudflare_ip("8.8.8.8", Some(CLOUDFLARE_ASN)));
        assert!(!is_cloudflare_ip("8.8.8.8", Some(15169)));
        assert!(!is_cloudflare_ip("203.0.113.1", None));
        assert!(!is_cloudflare_ip("不是 IP", None));
    }

    #[test]
    fn cf_落地只给机房三字码不硬编城市() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(GeoRecord::from_country("US", None))),
            Box::new(FixedOnline::broken()),
        );
        let exit = ExitIp::new("104.16.1.1")
            .with_trace(Some("US".into()), Some("lax".into()));
        let info = resolver.locate(&exit).unwrap();
        assert_eq!(info.three_parts(), ("Cloudflare".into(), "Cloudflare".into(), "LAX".into()));
        assert!(info.is_cloudflare());
        assert_eq!(info.country, "");

        // 没有 colo 时第三段留空，不硬编城市名。
        let without_colo = resolver
            .locate(&ExitIp::new("104.16.1.1"))
            .unwrap();
        assert_eq!(
            without_colo.three_parts(),
            ("Cloudflare".into(), "Cloudflare".into(), String::new())
        );
    }

    #[test]
    fn 非_cf_落地即使有_colo_也不显示机房() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let exit = ExitIp::new("203.0.113.9").with_trace(Some("CA".into()), Some("LAX".into()));
        let info = resolver.locate(&exit).unwrap();
        assert_eq!(info.three_parts(), ("加拿大".into(), "Canada".into(), String::new()));
        assert_eq!(info.colo, None);
        assert!(!info.is_cloudflare());
    }

    #[test]
    fn 离线库优先且在线源不被调用() {
        let online = std::sync::Arc::new(FixedOnline::ok(GeoRecord::from_country("US", None)));
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(OnlineProxy(online.clone())),
        );
        let info = resolver.locate(&ExitIp::new("203.0.113.9")).unwrap();
        assert_eq!(info.zh, "加拿大");
        assert_eq!(info.source, "offline");
        assert_eq!(online.calls(), 0);
    }

    #[test]
    fn 离线库缺失时用在线兜底() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::missing()),
            Box::new(FixedOnline::ok(GeoRecord::from_country("JP", Some(2497)))),
        );
        let info = resolver.locate(&ExitIp::new("203.0.113.9")).unwrap();
        assert_eq!(info.zh, "日本");
        assert_eq!(info.asn, Some(2497));
        assert_eq!(info.source, "online");
    }

    #[test]
    fn 两个源都失败时用_trace_的_loc_仍能显示国家() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::missing()),
            Box::new(FixedOnline::broken()),
        );
        let exit = ExitIp::new("203.0.113.9").with_trace(Some("SG".into()), None);
        let info = resolver.locate(&exit).unwrap();
        assert_eq!(info.three_parts(), ("新加坡".into(), "Singapore".into(), String::new()));
        assert_eq!(info.source, "trace");
    }

    #[test]
    fn 两个源都失败且没有_loc_时返回错误() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::missing()),
            Box::new(FixedOnline::broken()),
        );
        let error = resolver.locate(&ExitIp::new("203.0.113.9")).unwrap_err();
        assert!(error.contains("归属地查询失败"), "{error}");
    }

    #[test]
    fn 同一出口_ip_命中缓存不重复查询() {
        let offline = std::sync::Arc::new(FixedCountry::ok(canada()));
        let resolver = GeoResolver::with_lookups(
            Box::new(CountryProxy(offline.clone())),
            Box::new(FixedOnline::broken()),
        );
        let exit = ExitIp::new("203.0.113.9");
        let first = resolver.locate(&exit).unwrap();
        let second = resolver.locate(&exit).unwrap();

        assert_eq!(offline.calls(), 1, "第二次必须走缓存");
        assert_eq!(first.source, "offline");
        assert_eq!(second.source, "cache");
        assert_eq!(first.three_parts(), second.three_parts());
        assert_eq!(resolver.cache_len(), 1);
    }

    #[test]
    fn 落库的缓存可以预灌进内存() {
        let offline = std::sync::Arc::new(FixedCountry::ok(canada()));
        let resolver = GeoResolver::with_lookups(
            Box::new(CountryProxy(offline.clone())),
            Box::new(FixedOnline::broken()),
        );
        resolver.preload(vec![("203.0.113.9".to_string(), canada())]);
        let info = resolver.locate(&ExitIp::new("203.0.113.9")).unwrap();
        assert_eq!(info.source, "cache");
        assert_eq!(offline.calls(), 0);
    }

    #[test]
    fn 查询失败不清空已有的延迟与归属地() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_test_run(&crate::store::TestRunRecord {
                id: "run-geo",
                started_at: "2026-11-13T00:00:00.000Z",
                finished_at: "2026-11-13T00:00:01.000Z",
                params_json: "{}",
                total: 1,
                ok_count: 1,
                timeout_count: 0,
                fail_count: 0,
            })
            .unwrap();
        let endpoint = Endpoint::new("104.16.1.1", 8443);
        store
            .insert_test_result(
                "run-geo",
                &endpoint,
                "别名",
                &ProbeOutcome::ok(88),
            )
            .unwrap();

        // 第一次成功，写入归属地。
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let exit = ExitIp::new("203.0.113.9");
        let info = resolver.locate(&exit).unwrap();
        persist_geo(
            &store,
            "run-geo",
            &[GeoRow {
                endpoint: endpoint.clone(),
                alias: "别名".into(),
                exit_ip: Some(exit.ip.clone()),
                info: Some(info),
                error: None,
            }],
        )
        .unwrap();

        // 第二次失败：行里只有错误。
        persist_geo(
            &store,
            "run-geo",
            &[GeoRow {
                endpoint: endpoint.clone(),
                alias: "别名".into(),
                exit_ip: None,
                info: None,
                error: Some("归属地查询失败".into()),
            }],
        )
        .unwrap();

        let rows = store.list_test_results("run-geo").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].latency_ms, Some(88), "延迟不能被归属地清空");
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].exit_ip.as_deref(), Some("203.0.113.9"));
        assert_eq!(rows[0].geo_zh.as_deref(), Some("加拿大"));
        assert_eq!(rows[0].geo_en.as_deref(), Some("Canada"));
        assert_eq!(rows[0].geo_country.as_deref(), Some("CA"));
        // 同一 IP 的结论已进缓存表。
        let cache = store.list_geo_cache().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].ip, "203.0.113.9");
        assert_eq!(cache[0].record.zh, "加拿大");
    }

    #[test]
    fn 批量查询用假探测器给出固定出口_ip_并落库三段() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_test_run(&crate::store::TestRunRecord {
                id: "run-geo-batch",
                started_at: "2026-11-13T00:00:00.000Z",
                finished_at: "2026-11-13T00:00:01.000Z",
                params_json: "{}",
                total: 2,
                ok_count: 2,
                timeout_count: 0,
                fail_count: 0,
            })
            .unwrap();
        let items = vec![
            BatchItem::new(Endpoint::new("104.16.1.1", 8443), "第一条"),
            BatchItem::new(Endpoint::new("104.16.1.2", 8443), "第二条"),
        ];
        for item in &items {
            store
                .insert_test_result(
                    "run-geo-batch",
                    &item.endpoint,
                    &item.alias,
                    &ProbeOutcome::ok(66),
                )
                .unwrap();
        }

        let prober = FakeProber::scripted(vec![]).with_exits(vec![
            (
                ("104.16.1.1", 8443),
                Ok(ExitIp::new("104.16.1.1").with_trace(Some("US".into()), Some("LAX".into()))),
            ),
            (
                ("104.16.1.2", 8443),
                Ok(ExitIp::new("198.51.100.7").with_trace(Some("JP".into()), Some("NRT".into()))),
            ),
        ]);
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let report = run_geo_batch(
            &prober,
            &resolver,
            &items,
            &ProbeOptions::default(),
            2,
            &AtomicBool::new(false),
            &|_| {},
        );
        assert_eq!(report.counts().ok, 2);
        assert_eq!(prober.geo_calls().len(), 2, "每条地址只取一次出口 IP");
        persist_geo(&store, "run-geo-batch", &report.rows).unwrap();

        let rows = store.list_test_results("run-geo-batch").unwrap();
        assert_eq!(rows[0].latency_ms, Some(66), "归属地不能覆盖延迟");
        assert_eq!(rows[0].exit_ip.as_deref(), Some("104.16.1.1"));
        assert_eq!(rows[0].geo_zh.as_deref(), Some("Cloudflare"));
        assert_eq!(rows[0].colo.as_deref(), Some("LAX"));
        assert_eq!(rows[1].exit_ip.as_deref(), Some("198.51.100.7"));
        assert_eq!(rows[1].geo_zh.as_deref(), Some("加拿大"));
        assert_eq!(rows[1].geo_en.as_deref(), Some("Canada"));
        assert_eq!(rows[1].colo, None, "非 CF 落地不显示机房");
    }

    #[test]
    fn 批量查询里一条失败不阻塞其它行() {
        let items = vec![
            BatchItem::new(Endpoint::new("104.16.2.1", 8443), ""),
            BatchItem::new(Endpoint::new("104.16.2.2", 8443), ""),
        ];
        let prober = FakeProber::scripted(vec![]).with_exits(vec![
            (("104.16.2.1", 8443), Err("取出口 IP 失败".to_string())),
            (
                ("104.16.2.2", 8443),
                Ok(ExitIp::new("198.51.100.8").with_trace(Some("CA".into()), None)),
            ),
        ]);
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let report = run_geo_batch(
            &prober,
            &resolver,
            &items,
            &ProbeOptions::default(),
            2,
            &AtomicBool::new(false),
            &|_| {},
        );
        assert_eq!(report.completed, 2);
        assert!(report.rows[0].error.is_some());
        assert_eq!(report.rows[1].info.as_ref().unwrap().zh, "加拿大");
        assert_eq!(report.counts().ok, 1);
        assert_eq!(report.counts().fail, 1);
    }

    #[test]
    fn 同一出口_ip_的多条地址只查一次归属地() {
        let offline = std::sync::Arc::new(FixedCountry::ok(canada()));
        let resolver = GeoResolver::with_lookups(
            Box::new(CountryProxy(offline.clone())),
            Box::new(FixedOnline::broken()),
        );
        let items = vec![
            BatchItem::new(Endpoint::new("104.16.3.1", 8443), ""),
            BatchItem::new(Endpoint::new("104.16.3.2", 8443), ""),
        ];
        let prober = FakeProber::scripted(vec![]).with_exits(vec![
            (
                ("104.16.3.1", 8443),
                Ok(ExitIp::new("198.51.100.9").with_trace(Some("CA".into()), None)),
            ),
            (
                ("104.16.3.2", 8443),
                Ok(ExitIp::new("198.51.100.9").with_trace(Some("CA".into()), None)),
            ),
        ]);

        let report = run_geo_batch(
            &prober,
            &resolver,
            &items,
            &ProbeOptions::default(),
            4,
            &AtomicBool::new(false),
            &|_| {},
        );

        assert_eq!(report.counts().ok, 2);
        assert_eq!(offline.calls(), 1, "同一出口 IP 只能查一次");
        assert_eq!(report.rows[1].info.as_ref().unwrap().zh, "加拿大");
    }

    /// 把 Arc 包成 trait 对象，测试里既注入又保留计数句柄。
    struct CountryProxy(std::sync::Arc<FixedCountry>);
    impl CountryDb for CountryProxy {
        fn lookup(&self, ip: &str) -> Result<GeoRecord, String> {
            self.0.lookup(ip)
        }
    }

    struct OnlineProxy(std::sync::Arc<FixedOnline>);
    impl OnlineGeo for OnlineProxy {
        fn lookup(&self, ip: &str) -> Result<GeoRecord, String> {
            self.0.lookup(ip)
        }
    }
}
