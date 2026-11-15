#![cfg_attr(not(test), allow(dead_code))]
//! 归属地：查优选地址本身的 IP（见 ADR-0007）。
//!
//! 直连探测不再有「出口 IP」这个概念，归属地只说明这条地址本身注册在哪个国家。
//! 查询优先用内置离线库，`ipwho.is` 只作兜底；中英文名统一走 `countries` 的本地表，
//! 在线源的中文译名一律不用，避免繁简混排。

pub mod countries;
pub mod offline;
pub mod online;

pub use countries::country_name;
pub use offline::OfflineDb;
pub use online::IpWhoIs;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::probe::batch::{BatchEvent, BatchItem, BatchProgress};
use crate::probe::{Endpoint, HostResolver, ProbeStatus};
use crate::store::Store;

/// 离线库候选路径：主程序同目录 → 同目录 resources → 开发期 resources。
///
/// 子进程（`--probe-worker`）没有 Tauri 资源目录，只能靠这几处定位离线库。
pub fn offline_candidates() -> Vec<PathBuf> {
    let file = offline::GEOIP_FILE;
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(offline::GEOIP_DIR).join(file));
            candidates.push(dir.join("resources").join(offline::GEOIP_DIR).join(file));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(offline::GEOIP_DIR)
            .join(file),
    );
    candidates
}

/// 一条归属地记录（缓存与落库的最小单位）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoRecord {
    /// ISO 国家码。
    pub country: String,
    pub zh: String,
    pub en: String,
    /// 离线库给不出，在线查询才可能有。
    pub asn: Option<u32>,
}

impl GeoRecord {
    /// 按国家码构造，中英文名走本地表。
    #[cfg_attr(not(test), allow(dead_code))] // 在线源与测试用
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

/// 一条地址的归属地结论，界面两段显示就用它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoInfo {
    /// 这条地址解析出的 IP，也就是被查询的对象。
    pub ip: String,
    pub country: String,
    pub zh: String,
    pub en: String,
    pub asn: Option<u32>,
    /// 数据来源：`offline` / `online` / `cache`。
    pub source: String,
}

impl GeoInfo {
    /// 「中文 / English」两段。
    #[cfg_attr(not(test), allow(dead_code))] // 导出与测试用
    pub fn two_parts(&self) -> (String, String) {
        (self.zh.clone(), self.en.clone())
    }

    /// 一段式展示，导出与日志用。
    #[allow(dead_code)]
    pub fn display(&self) -> String {
        let (zh, en) = self.two_parts();
        format!("{zh} / {en}")
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

    /// 查一个 IP 的归属地：内存缓存 → 离线库 → 在线兜底。
    ///
    /// 全部失败才返回错误；调用方据此只留空归属地两列，不动延迟与速度。
    pub fn locate(&self, ip: &str) -> Result<GeoInfo, String> {
        let ip = ip.trim();
        if ip.is_empty() {
            return Err("地址为空，无法查归属地".to_string());
        }

        let (record, source) = match self.cached(ip) {
            Some(record) => (record, "cache"),
            None => {
                let (record, source) = match self.offline.lookup(ip) {
                    Ok(record) => (record, "offline"),
                    Err(offline_error) => match self.online.lookup(ip) {
                        Ok(record) => (record, "online"),
                        Err(online_error) => {
                            return Err(format!(
                                "归属地查询失败：{offline_error}；{online_error}"
                            ))
                        }
                    },
                };
                self.remember(ip, &record);
                (record, source)
            }
        };

        Ok(GeoInfo {
            ip: ip.to_string(),
            country: record.country,
            zh: record.zh,
            en: record.en,
            asn: record.asn,
            source: source.to_string(),
        })
    }
}

/// 一条地址的归属地结果。
#[derive(Debug, Clone, PartialEq)]
pub struct GeoRow {
    pub endpoint: Endpoint,
    pub alias: String,
    /// 这条地址解析出的 IP；解析失败时为 `None`。
    pub ip: Option<String>,
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
/// 每条地址先解析出 IP，再查该 IP 的归属地；同一 IP 命中缓存时不重复查询。
/// 每完成一条推一次逐行事件（`countryZh` / `countryEn`），再推计数进度。
pub fn run_geo_batch(
    resolver: &dyn HostResolver,
    geo: &GeoResolver,
    items: &[BatchItem],
    concurrency: usize,
    cancel: &AtomicBool,
    on_event: &(dyn Fn(BatchEvent) + Sync),
) -> GeoReport {
    let total = items.len();
    let concurrency = concurrency.clamp(1, 32);

    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let rows: Vec<Mutex<Option<GeoRow>>> = (0..total).map(|_| Mutex::new(None)).collect();

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

                let item = &items[index];
                let mut row = GeoRow {
                    endpoint: item.endpoint.clone(),
                    alias: item.alias.clone(),
                    ip: None,
                    info: None,
                    error: None,
                };
                match resolver.resolve(&item.endpoint) {
                    Ok(ip) => {
                        let ip = ip.to_string();
                        match geo.locate(&ip) {
                            Ok(info) => row.info = Some(info),
                            Err(error) => row.error = Some(error),
                        }
                        row.ip = Some(ip);
                    }
                    Err(error) => row.error = Some(error),
                }

                // 一行结论成型就推一次；失败行只带错误，不清空界面已有的归属地。
                let status = if row.info.is_some() {
                    ProbeStatus::Ok
                } else if row.error.is_some() {
                    ProbeStatus::Fail
                } else {
                    ProbeStatus::Untested
                };
                let mut event = crate::probe::RowEvent::new(&item.endpoint, status);
                event.error = row.error.clone();
                if let Some(info) = &row.info {
                    event.country_zh = Some(info.zh.clone());
                    event.country_en = Some(info.en.clone());
                    event.country = Some(info.country.clone());
                }
                on_event(BatchEvent::Row(event));

                if let Ok(mut slot) = rows[index].lock() {
                    *slot = Some(row);
                }
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                on_event(BatchEvent::Progress(BatchProgress {
                    completed: done,
                    total,
                }));
            });
        }
    });

    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            rows[index]
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| GeoRow {
                    endpoint: item.endpoint.clone(),
                    alias: item.alias.clone(),
                    ip: None,
                    info: None,
                    error: None,
                })
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
                &info.ip,
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
    fn 离线库优先且在线源不被调用() {
        let online = std::sync::Arc::new(FixedOnline::ok(GeoRecord::from_country("US", None)));
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(OnlineProxy(online.clone())),
        );
        let info = resolver.locate("203.0.113.9").unwrap();
        assert_eq!(info.two_parts(), ("加拿大".into(), "Canada".into()));
        assert_eq!(info.ip, "203.0.113.9");
        assert_eq!(info.source, "offline");
        assert_eq!(online.calls(), 0);
    }

    #[test]
    fn 离线库缺失时用在线兜底() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::missing()),
            Box::new(FixedOnline::ok(GeoRecord::from_country("JP", Some(2497)))),
        );
        let info = resolver.locate("203.0.113.9").unwrap();
        assert_eq!(info.zh, "日本");
        assert_eq!(info.asn, Some(2497));
        assert_eq!(info.source, "online");
    }

    #[test]
    fn 两个源都失败时返回错误() {
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::missing()),
            Box::new(FixedOnline::broken()),
        );
        let error = resolver.locate("203.0.113.9").unwrap_err();
        assert!(error.contains("归属地查询失败"), "{error}");
        assert!(resolver.locate("  ").is_err());
    }

    #[test]
    fn 同一_ip_命中缓存不重复查询() {
        let offline = std::sync::Arc::new(FixedCountry::ok(canada()));
        let resolver = GeoResolver::with_lookups(
            Box::new(CountryProxy(offline.clone())),
            Box::new(FixedOnline::broken()),
        );
        let first = resolver.locate("203.0.113.9").unwrap();
        let second = resolver.locate("203.0.113.9").unwrap();

        assert_eq!(offline.calls(), 1, "第二次必须走缓存");
        assert_eq!(first.source, "offline");
        assert_eq!(second.source, "cache");
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
        let info = resolver.locate("203.0.113.9").unwrap();
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
            .insert_test_result("run-geo", &endpoint, "别名", &ProbeOutcome::ok(88))
            .unwrap();

        // 第一次成功，写入归属地。
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let info = resolver.locate("104.16.1.1").unwrap();
        persist_geo(
            &store,
            "run-geo",
            &[GeoRow {
                endpoint: endpoint.clone(),
                alias: "别名".into(),
                ip: Some(info.ip.clone()),
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
                ip: None,
                info: None,
                error: Some("归属地查询失败".into()),
            }],
        )
        .unwrap();

        let rows = store.list_test_results("run-geo").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].latency_ms, Some(88), "延迟不能被归属地清空");
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].geo_zh.as_deref(), Some("加拿大"));
        assert_eq!(rows[0].geo_en.as_deref(), Some("Canada"));
        assert_eq!(rows[0].geo_country.as_deref(), Some("CA"));
        // 同一 IP 的结论已进缓存表。
        let cache = store.list_geo_cache().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].ip, "104.16.1.1");
        assert_eq!(cache[0].record.zh, "加拿大");
    }

    #[test]
    fn 批量查询查地址本身的_ip_并落库两段() {
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
            BatchItem::new(Endpoint::new("cdn.example.com", 443), "第二条"),
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

        let prober = FakeProber::default().with_ips(vec![(
            ("cdn.example.com", 443),
            Ok("198.51.100.7".parse().unwrap()),
        )]);
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let events = Mutex::new(Vec::new());
        let report = run_geo_batch(
            &prober,
            &resolver,
            &items,
            2,
            &AtomicBool::new(false),
            &|event| events.lock().unwrap().push(event),
        );
        assert_eq!(report.counts().ok, 2);
        assert_eq!(report.rows[0].ip.as_deref(), Some("104.16.1.1"));
        assert_eq!(report.rows[1].ip.as_deref(), Some("198.51.100.7"));
        persist_geo(&store, "run-geo-batch", &report.rows).unwrap();

        let rows = store.list_test_results("run-geo-batch").unwrap();
        assert_eq!(rows[0].latency_ms, Some(66), "归属地不能覆盖延迟");
        assert_eq!(rows[0].geo_zh.as_deref(), Some("加拿大"));
        assert_eq!(rows[0].geo_en.as_deref(), Some("Canada"));

        // 逐行事件带中文名与英文名，机房恒为 null。
        let rows: Vec<crate::probe::RowEvent> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                BatchEvent::Row(row) => Some(row.clone()),
                BatchEvent::Progress(_) => None,
            })
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].country_zh.as_deref(), Some("加拿大"));
        assert_eq!(rows[0].country_en.as_deref(), Some("Canada"));
        assert_eq!(rows[0].colo, None);
    }

    #[test]
    fn 批量查询里一条失败不阻塞其它行() {
        let items = vec![
            BatchItem::new(Endpoint::new("104.16.2.1", 8443), ""),
            BatchItem::new(Endpoint::new("104.16.2.2", 8443), ""),
        ];
        let prober = FakeProber::default().with_ips(vec![(
            ("104.16.2.1", 8443),
            Err("解析失败".to_string()),
        )]);
        let resolver = GeoResolver::with_lookups(
            Box::new(FixedCountry::ok(canada())),
            Box::new(FixedOnline::broken()),
        );
        let report = run_geo_batch(
            &prober,
            &resolver,
            &items,
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
