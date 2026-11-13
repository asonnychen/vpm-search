//! 经代理链路取出口 IP：两个端点交叉校验，取多数值。
//!
//! 端点选择见 ADR-0002：
//! - `api.ipify.org` 只给 IP；
//! - `www.cloudflare.com/cdn-cgi/trace` 同时给 `ip`、`loc`、`colo`。
//!
//! 单个端点失败不影响结论；两个都失败才算失败。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{pick_exit_ip, ExitIp, ExitSample};

/// 只返回 IP 的端点。
pub const IPIFY_URL: &str = "https://api.ipify.org?format=json";
/// 同时给出 ip / loc / colo 的端点。
pub const TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

/// 解析 `api.ipify.org?format=json` 的响应。
pub fn parse_ipify(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let ip = value.get("ip")?.as_str()?.trim();
    if ip.is_empty() {
        None
    } else {
        Some(ip.to_string())
    }
}

/// 解析 `cdn-cgi/trace` 的 `key=value` 行。
pub fn parse_trace(body: &str) -> ExitSample {
    let mut sample = ExitSample::default();
    for line in body.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "ip" => sample.ip = Some(value.to_string()),
            "loc" => sample.loc = Some(value.to_uppercase()),
            "colo" => sample.colo = Some(value.to_uppercase()),
            _ => {}
        }
    }
    sample
}

/// 经代理（`proxy` 为空时直连）请求两个端点并取多数。
///
/// 返回的 `asn` 由归属地查询补齐，这里恒为 `None`。
pub fn fetch_exit_ip(
    proxy: Option<&str>,
    connect_timeout: Duration,
    request_timeout: Duration,
    cancel: &AtomicBool,
) -> Result<ExitIp, String> {
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout);
    if let Some(proxy) = proxy {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy).map_err(|e| format!("本地代理地址无效：{e}"))?,
        );
    }
    let client = builder
        .build()
        .map_err(|e| format!("创建出口 IP 客户端失败：{e}"))?;

    let mut samples = Vec::with_capacity(2);
    let mut errors = Vec::new();

    if cancel.load(Ordering::SeqCst) {
        return Err("归属地查询已停止".to_string());
    }
    match client.get(IPIFY_URL).send() {
        Ok(response) if response.status().is_success() => match response.text() {
            Ok(body) => match parse_ipify(&body) {
                Some(ip) => samples.push(ExitSample {
                    ip: Some(ip),
                    loc: None,
                    colo: None,
                }),
                None => errors.push("ipify 返回了无法解析的内容".to_string()),
            },
            Err(error) => errors.push(format!("ipify 读取失败：{error}")),
        },
        Ok(response) => errors.push(format!("ipify 返回 HTTP {}", response.status().as_u16())),
        Err(error) => errors.push(format!("ipify 请求失败：{error}")),
    }

    if cancel.load(Ordering::SeqCst) {
        return Err("归属地查询已停止".to_string());
    }
    match client.get(TRACE_URL).send() {
        Ok(response) if response.status().is_success() => match response.text() {
            Ok(body) => {
                let sample = parse_trace(&body);
                if sample.ip.is_some() {
                    samples.push(sample);
                } else {
                    errors.push("cdn-cgi/trace 没有返回 ip".to_string());
                }
            }
            Err(error) => errors.push(format!("cdn-cgi/trace 读取失败：{error}")),
        },
        Ok(response) => errors.push(format!(
            "cdn-cgi/trace 返回 HTTP {}",
            response.status().as_u16()
        )),
        Err(error) => errors.push(format!("cdn-cgi/trace 请求失败：{error}")),
    }

    pick_exit_ip(&samples).ok_or_else(|| {
        if errors.is_empty() {
            "两个端点都没有给出出口 IP".to_string()
        } else {
            format!("取出口 IP 失败：{}", errors.join("；"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析_ipify_的_json() {
        assert_eq!(
            parse_ipify(r#"{"ip":"203.0.113.7"}"#),
            Some("203.0.113.7".to_string())
        );
        assert_eq!(parse_ipify("not json"), None);
        assert_eq!(parse_ipify(r#"{"ip":""}"#), None);
    }

    #[test]
    fn 解析_trace_的_ip_国家与机房() {
        let body = "fl=123abc\nh=www.cloudflare.com\nip=203.0.113.9\nts=1\ncolo=LAX\nloc=us\n";
        let sample = parse_trace(body);
        assert_eq!(sample.ip.as_deref(), Some("203.0.113.9"));
        assert_eq!(sample.loc.as_deref(), Some("US"));
        assert_eq!(sample.colo.as_deref(), Some("LAX"));
    }

    #[test]
    fn trace_缺字段时只留空对应项() {
        let sample = parse_trace("ip=203.0.113.9\ncolo=\n");
        assert_eq!(sample.ip.as_deref(), Some("203.0.113.9"));
        assert_eq!(sample.loc, None);
        assert_eq!(sample.colo, None);
    }
}
