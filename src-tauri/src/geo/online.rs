//! 在线兜底：`ipwho.is`（HTTPS、免费额度 1000 次/天、允许商用）。
//!
//! 只用它拿国家码与 ASN；中文名一律走本地映射表，避免在线源返回的
//! 繁体或异体译名混进界面（ADR-0002）。
//!
//! 这是「查询归属地」，与「经代理取出口 IP」不同：它直连，不经过代理链路。

use std::sync::OnceLock;
use std::time::Duration;

use super::countries::country_name;
use super::{GeoRecord, OnlineGeo};

/// 免费接口的根地址。
pub const IPWHOIS_BASE: &str = "https://ipwho.is";
/// 单次查询超时。
pub const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);

/// 解析 `ipwho.is` 的响应。
pub fn parse_ipwhois(body: &str) -> Result<GeoRecord, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("在线归属地返回了无法解析的内容：{e}"))?;

    if value.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("在线归属地查询失败");
        return Err(message.to_string());
    }

    let code = value
        .get("country_code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_uppercase();
    if code.is_empty() {
        return Err("在线归属地没有给出国家码".to_string());
    }

    let asn = value
        .get("connection")
        .and_then(|connection| connection.get("asn"))
        .and_then(|asn| asn.as_u64())
        .map(|asn| asn as u32);

    let (zh, en) = country_name(&code);
    Ok(GeoRecord {
        country: code,
        zh,
        en,
        asn,
    })
}

/// 直连 `ipwho.is` 的在线查询器。
pub struct IpWhoIs {
    base: String,
    client: OnceLock<reqwest::blocking::Client>,
}

impl Default for IpWhoIs {
    fn default() -> Self {
        Self::new()
    }
}

impl IpWhoIs {
    pub fn new() -> Self {
        Self::with_base(IPWHOIS_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: OnceLock::new(),
        }
    }

    fn client(&self) -> Result<&reqwest::blocking::Client, String> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .connect_timeout(ONLINE_TIMEOUT)
            .timeout(ONLINE_TIMEOUT)
            .build()
            .map_err(|e| format!("创建在线归属地客户端失败：{e}"))?;
        let _ = self.client.set(client);
        self.client
            .get()
            .ok_or_else(|| "在线归属地客户端不可用".to_string())
    }
}

impl OnlineGeo for IpWhoIs {
    fn lookup(&self, ip: &str) -> Result<GeoRecord, String> {
        let client = self.client()?;
        let url = format!("{}/{ip}", self.base.trim_end_matches('/'));
        let response = client
            .get(&url)
            .send()
            .map_err(|e| format!("在线归属地请求失败：{e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "在线归属地返回 HTTP {}",
                response.status().as_u16()
            ));
        }
        let body = response
            .text()
            .map_err(|e| format!("在线归属地读取失败：{e}"))?;
        parse_ipwhois(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析在线响应得到国家码与_asn() {
        let body = r#"{"ip":"203.0.113.7","success":true,"country_code":"CA","connection":{"asn":13335,"isp":"Cloudflare"}}"#;
        let record = parse_ipwhois(body).unwrap();
        assert_eq!(record.country, "CA");
        assert_eq!(record.zh, "加拿大");
        assert_eq!(record.en, "Canada");
        assert_eq!(record.asn, Some(13335));
    }

    #[test]
    fn 在线失败时给出可读错误() {
        let error = parse_ipwhois(r#"{"success":false,"message":"invalid query"}"#).unwrap_err();
        assert_eq!(error, "invalid query");
        assert!(parse_ipwhois("不是 json").is_err());
        assert!(parse_ipwhois(r#"{"success":true}"#).is_err());
    }
}
