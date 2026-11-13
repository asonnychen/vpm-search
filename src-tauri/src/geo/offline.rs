//! 内置离线库：DB-IP Country Lite（MMDB，CC BY 4.0）。
//!
//! 文件由 `scripts/fetch-geoip.ps1` 下载到 `src-tauri/resources/geoip/`，
//! 打包时随应用一起分发；运行时按「主程序同目录 → 资源目录 → 开发期 resources」
//! 的顺序查找。离线库只给国家码，中英文名统一走本地映射表。
//!
//! 若文件缺失或损坏，查询会返回错误，调用方回落到在线源；代码里不假装有离线数据。

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::OnceLock;

use maxminddb::Reader;
use serde::Deserialize;

use super::countries::country_name;
use super::{CountryDb, GeoRecord};

/// 离线库在打包产物与开发目录里的相对位置。
pub const GEOIP_DIR: &str = "geoip";
pub const GEOIP_FILE: &str = "dbip-country-lite.mmdb";

/// DB-IP / MaxMind 的 country 记录，只取需要的字段。
#[derive(Debug, Deserialize)]
struct DbRecord {
    country: Option<DbCountry>,
    registered_country: Option<DbCountry>,
}

#[derive(Debug, Deserialize)]
struct DbCountry {
    iso_code: Option<String>,
}

/// 懒加载的 MMDB 读取器。
pub struct OfflineDb {
    candidates: Vec<PathBuf>,
    reader: OnceLock<Option<Reader<Vec<u8>>>>,
    loaded: OnceLock<Option<PathBuf>>,
}

impl OfflineDb {
    pub fn new(candidates: Vec<PathBuf>) -> Self {
        Self {
            candidates,
            reader: OnceLock::new(),
            loaded: OnceLock::new(),
        }
    }

    fn reader(&self) -> Option<&Reader<Vec<u8>>> {
        self.reader
            .get_or_init(|| {
                for candidate in &self.candidates {
                    if !candidate.is_file() {
                        continue;
                    }
                    if let Ok(reader) = Reader::open_readfile(candidate) {
                        let _ = self.loaded.set(Some(candidate.clone()));
                        return Some(reader);
                    }
                }
                None
            })
            .as_ref()
    }

    /// 实际加载到的文件路径；未加载时为 `None`。日志与诊断用。
    #[allow(dead_code)] // 诊断接口，阶段 11 的库信息会用到
    pub fn loaded_path(&self) -> Option<&PathBuf> {
        self.loaded.get().and_then(|path| path.as_ref())
    }

    /// 是否找到了可用的离线库（会触发一次懒加载）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn available(&self) -> bool {
        self.reader().is_some()
    }

    fn describe(&self) -> String {
        self.candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    }
}

impl CountryDb for OfflineDb {
    fn lookup(&self, ip: &str) -> Result<GeoRecord, String> {
        let Some(reader) = self.reader() else {
            return Err(format!("未找到离线归属地库，已尝试：{}", self.describe()));
        };
        let address: IpAddr = ip
            .parse()
            .map_err(|_| format!("出口 IP 不是合法地址：{ip}"))?;
        let lookup = reader
            .lookup(address)
            .map_err(|e| format!("离线库查询失败：{e}"))?;
        if !lookup.has_data() {
            return Err(format!("离线库里没有 {ip} 的记录"));
        }
        let record: Option<DbRecord> = lookup
            .decode()
            .map_err(|e| format!("离线库记录解析失败：{e}"))?;
        let code = record
            .and_then(|record| record.country.or(record.registered_country))
            .and_then(|country| country.iso_code)
            .unwrap_or_default();
        if code.trim().is_empty() {
            return Err(format!("离线库里 {ip} 没有国家码"));
        }
        let (zh, en) = country_name(&code);
        Ok(GeoRecord {
            country: code.to_uppercase(),
            zh,
            en,
            asn: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没有库文件时返回可读错误而不是空结果() {
        let db = OfflineDb::new(vec![PathBuf::from("Z:/不存在/geoip.mmdb")]);
        assert!(!db.available());
        let error = db.lookup("1.1.1.1").unwrap_err();
        assert!(error.contains("离线归属地库"), "{error}");
    }

    #[test]
    fn 非法_ip_被拒绝() {
        let db = OfflineDb::new(vec![PathBuf::from("Z:/不存在/geoip.mmdb")]);
        assert!(db.lookup("不是 IP").is_err());
    }

    #[test]
    fn 内置库能解析已知_ip_的国家() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(GEOIP_DIR)
            .join(GEOIP_FILE);
        if !path.is_file() {
            // 没执行 scripts/fetch-geoip.ps1 时跳过，不让测试因此失败。
            return;
        }

        let db = OfflineDb::new(vec![path]);
        assert!(db.available());

        // 8.8.8.8 在 DB-IP Country Lite 里归属美国。
        let record = db.lookup("8.8.8.8").unwrap();
        assert_eq!(record.country, "US");
        assert_eq!(record.zh, "美国");
        assert_eq!(record.en, "United States");

        // 离线库只给国家码，没有 ASN。
        assert_eq!(record.asn, None);
    }
}
