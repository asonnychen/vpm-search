//! 分享链接解析：`vless://` / `vmess://` / `trojan://` / `ss://`。
//!
//! 纯字符串转换，不碰网络与数据库。任何字段缺失或格式错误都返回可读错误，
//! 不产出半成品模板。

use serde_json::Value;

use super::{NodeTemplate, TemplateError, KINDS, TRANSPORTS};

fn err(message: impl Into<String>) -> TemplateError {
    TemplateError(message.into())
}

/// 把一条分享链接解析成节点模板。`id` 留空，由命令层分配。
pub fn parse_link(link: &str) -> Result<NodeTemplate, TemplateError> {
    let link = link.trim();
    if link.is_empty() {
        return Err(err("链接为空，请粘贴一条分享链接"));
    }
    let (scheme, rest) = link.split_once("://").ok_or_else(|| {
        err("链接缺少协议前缀，应以 vless:// / vmess:// / trojan:// / ss:// 开头")
    })?;
    match scheme.to_ascii_lowercase().as_str() {
        "vless" => parse_vless(rest),
        "vmess" => parse_vmess(rest),
        "trojan" => parse_trojan(rest),
        "ss" => parse_ss(rest),
        other => Err(err(format!(
            "不支持的协议「{other}」，只支持 vless / vmess / trojan / ss"
        ))),
    }
}

/// 通用 URI 结构：凭据、服务器、端口、备注与查询参数。
struct UriParts {
    userinfo: String,
    host: String,
    port: u16,
    name: String,
    /// 键名统一小写。
    query: Vec<(String, String)>,
}

impl UriParts {
    fn get(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// 拆出 `#备注`、`?查询` 与 `凭据@host:port`。
fn split_uri(rest: &str) -> Result<UriParts, TemplateError> {
    let (body, fragment) = match rest.split_once('#') {
        Some((body, fragment)) => (body, percent_decode(fragment)),
        None => (rest, String::new()),
    };
    let (authority, query) = match body.split_once('?') {
        Some((authority, query)) => (authority, parse_query(query)),
        None => (body, Vec::new()),
    };
    let (userinfo, hostport) = authority.rsplit_once('@').ok_or_else(|| {
        err("链接缺少用户凭据（用户 ID 或密码），格式应为 scheme://凭据@host:port")
    })?;
    let (host, port) = split_host_port(hostport)?;
    Ok(UriParts {
        userinfo: percent_decode(userinfo),
        host,
        port,
        name: fragment.trim().to_string(),
        query,
    })
}

/// 拆分 `host:port`，兼容 `[IPv6]:port`。
fn split_host_port(value: &str) -> Result<(String, u16), TemplateError> {
    let value = value.trim().trim_end_matches('/');
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| err("IPv6 地址缺少右括号"))?;
        let host = &rest[..end];
        let after = &rest[end + 1..];
        let port = after
            .strip_prefix(':')
            .ok_or_else(|| err("链接缺少端口，格式应为 host:port"))?;
        (host, port)
    } else {
        value
            .rsplit_once(':')
            .ok_or_else(|| err("链接缺少端口，格式应为 host:port"))?
    };

    if host.trim().is_empty() {
        return Err(err("链接缺少服务器地址"));
    }
    let port: u16 = port.trim().parse().map_err(|_| {
        err(format!("端口「{}」不是 1–65535 的整数", port.trim()))
    })?;
    if port == 0 {
        return Err(err("端口必须在 1–65535 之间"));
    }
    Ok((host.trim().to_string(), port))
}

/// 解析查询串，键名转小写，值做百分号解码。
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (key.to_ascii_lowercase(), percent_decode(value)),
            None => (pair.to_ascii_lowercase(), String::new()),
        })
        .collect()
}

/// `%XX` 解码；非法转义原样保留。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 传输方式归一化，拒绝未知值而不是悄悄降级。
fn transport_of(raw: &str) -> Result<String, TemplateError> {
    let lower = raw.trim().to_ascii_lowercase();
    let transport = if lower.is_empty() {
        "tcp".to_string()
    } else {
        lower
    };
    if TRANSPORTS.contains(&transport.as_str()) {
        Ok(transport)
    } else {
        Err(err(format!(
            "不支持的传输方式「{transport}」，可选 {}",
            TRANSPORTS.join(" / ")
        )))
    }
}

/// TLS 由 `security` 参数决定，缺省按协议自己的默认值。
fn tls_of(security: Option<&str>, default: bool) -> bool {
    match security.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if value == "none" || value.is_empty() => false,
        Some(value) => matches!(value.as_str(), "tls" | "reality" | "xtls"),
        None => default,
    }
}

fn parse_vless(rest: &str) -> Result<NodeTemplate, TemplateError> {
    let parts = split_uri(rest)?;
    if parts.userinfo.trim().is_empty() {
        return Err(err("vless 链接缺少用户 ID（UUID）"));
    }
    let transport = transport_of(parts.get("type").unwrap_or(""))?;
    let tls = tls_of(parts.get("security"), false);
    let template = NodeTemplate {
        id: String::new(),
        name: parts.name.clone(),
        kind: "vless".to_string(),
        server: parts.host.clone(),
        port: parts.port,
        uuid: parts.userinfo.clone(),
        password: String::new(),
        method: String::new(),
        transport: transport.clone(),
        tls,
        sni: parts.get("sni").or_else(|| parts.get("peer")).unwrap_or("").to_string(),
        host: parts.get("host").unwrap_or("").to_string(),
        path: parts.get("path").unwrap_or("").to_string(),
        service_name: if transport == "grpc" {
            parts.get("servicename").unwrap_or("").to_string()
        } else {
            String::new()
        },
    };
    finish(template)
}

fn parse_trojan(rest: &str) -> Result<NodeTemplate, TemplateError> {
    let parts = split_uri(rest)?;
    if parts.userinfo.trim().is_empty() {
        return Err(err("trojan 链接缺少密码"));
    }
    let transport = transport_of(parts.get("type").unwrap_or(""))?;
    let template = NodeTemplate {
        id: String::new(),
        name: parts.name.clone(),
        kind: "trojan".to_string(),
        server: parts.host.clone(),
        port: parts.port,
        uuid: String::new(),
        password: parts.userinfo.clone(),
        method: String::new(),
        transport: transport.clone(),
        // trojan 默认走 TLS，只有显式 security=none 才关闭。
        tls: tls_of(parts.get("security"), true),
        sni: parts.get("sni").or_else(|| parts.get("peer")).unwrap_or("").to_string(),
        host: parts.get("host").unwrap_or("").to_string(),
        path: parts.get("path").unwrap_or("").to_string(),
        service_name: if transport == "grpc" {
            parts.get("servicename").unwrap_or("").to_string()
        } else {
            String::new()
        },
    };
    finish(template)
}

fn parse_ss(rest: &str) -> Result<NodeTemplate, TemplateError> {
    let (body, fragment) = match rest.split_once('#') {
        Some((body, fragment)) => (body, percent_decode(fragment)),
        None => (rest, String::new()),
    };
    // ss 的插件参数（?plugin=…）本阶段不支持，忽略即可。
    let authority = body.split('?').next().unwrap_or(body).trim().trim_end_matches('/');

    let (credentials, host, port) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => {
            let (host, port) = split_host_port(hostport)?;
            (decode_ss_userinfo(userinfo)?, host, port)
        }
        None => {
            let decoded = base64_decode(authority).map_err(|_| {
                err("ss 链接的凭据不是有效的 base64，格式应为 ss://base64(method:password)@host:port")
            })?;
            let decoded = String::from_utf8(decoded)
                .map_err(|_| err("ss 链接的 base64 内容不是 UTF-8 文本"))?;
            let (userinfo, hostport) = decoded
                .rsplit_once('@')
                .ok_or_else(|| err("ss 链接缺少服务器地址，格式应为 method:password@host:port"))?;
            let (host, port) = split_host_port(hostport)?;
            (userinfo.to_string(), host, port)
        }
    };

    let (method, password) = credentials
        .split_once(':')
        .ok_or_else(|| err("ss 凭据格式应为 加密方式:密码"))?;
    if method.trim().is_empty() {
        return Err(err("ss 链接缺少加密方式"));
    }
    if password.is_empty() {
        return Err(err("ss 链接缺少密码"));
    }

    finish(NodeTemplate {
        id: String::new(),
        name: fragment.trim().to_string(),
        kind: "ss".to_string(),
        server: host,
        port,
        uuid: String::new(),
        password: password.to_string(),
        method: method.trim().to_string(),
        transport: "tcp".to_string(),
        tls: false,
        sni: String::new(),
        host: String::new(),
        path: String::new(),
        service_name: String::new(),
    })
}

/// SIP002 的 userinfo 可能是 base64，也可能直接是 `method:password`。
fn decode_ss_userinfo(userinfo: &str) -> Result<String, TemplateError> {
    match base64_decode(userinfo) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) if text.contains(':') => Ok(text),
            _ => Ok(percent_decode(userinfo)),
        },
        Err(()) => {
            if userinfo.contains(':') {
                Ok(percent_decode(userinfo))
            } else {
                Err(err("ss 链接的用户信息既不是 base64，也不是 加密方式:密码"))
            }
        }
    }
}

fn parse_vmess(rest: &str) -> Result<NodeTemplate, TemplateError> {
    let (body, fragment) = match rest.split_once('#') {
        Some((body, fragment)) => (body, percent_decode(fragment)),
        None => (rest, String::new()),
    };
    let payload = body.split('?').next().unwrap_or(body).trim();
    let decoded = base64_decode(payload)
        .map_err(|_| err("vmess 链接的 base64 内容无法解码，请检查是否复制完整"))?;
    let text = String::from_utf8(decoded)
        .map_err(|_| err("vmess 链接的 base64 内容不是 UTF-8 文本"))?;
    let value: Value = serde_json::from_str(text.trim())
        .map_err(|e| err(format!("vmess 链接内容不是合法 JSON：{e}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| err("vmess 链接内容应为 JSON 对象"))?;

    let text_field = |key: &str| -> String {
        match object.get(key) {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            Some(Value::Bool(value)) => value.to_string(),
            _ => String::new(),
        }
    };

    let server = text_field("add");
    if server.trim().is_empty() {
        return Err(err("vmess 链接缺少服务器地址（add）"));
    }
    let port_text = text_field("port");
    let port: u16 = port_text
        .trim()
        .parse()
        .map_err(|_| err(format!("vmess 端口「{port_text}」不是 1–65535 的整数")))?;
    if port == 0 {
        return Err(err("vmess 端口必须在 1–65535 之间"));
    }
    let uuid = text_field("id");
    if uuid.trim().is_empty() {
        return Err(err("vmess 链接缺少用户 ID（id）"));
    }

    let transport = transport_of(&text_field("net"))?;
    let tls = matches!(
        text_field("tls").trim().to_ascii_lowercase().as_str(),
        "tls" | "reality" | "xtls" | "true"
    );
    let sni = {
        let sni = text_field("sni");
        if sni.is_empty() {
            text_field("host")
        } else {
            sni
        }
    };
    let path = text_field("path");

    let name = {
        let ps = text_field("ps");
        if !ps.trim().is_empty() {
            ps
        } else if !fragment.trim().is_empty() {
            fragment.trim().to_string()
        } else {
            String::new()
        }
    };

    finish(NodeTemplate {
        id: String::new(),
        name,
        kind: "vmess".to_string(),
        server: server.trim().to_string(),
        port,
        uuid: uuid.trim().to_string(),
        password: String::new(),
        method: text_field("scy"),
        transport: transport.clone(),
        tls,
        sni,
        host: text_field("host"),
        path: if transport == "grpc" {
            String::new()
        } else {
            path.clone()
        },
        service_name: if transport == "grpc" {
            let service = text_field("serviceName");
            if service.is_empty() {
                path
            } else {
                service
            }
        } else {
            String::new()
        },
    })
}

/// 补齐名称并做最终校验，确保返回的模板可以直接保存。
fn finish(mut template: NodeTemplate) -> Result<NodeTemplate, TemplateError> {
    if !KINDS.contains(&template.kind.as_str()) {
        return Err(err(format!("不支持的协议类型「{}」", template.kind)));
    }
    if !TRANSPORTS.contains(&template.transport.as_str()) {
        return Err(err(format!(
            "不支持的传输方式「{}」，可选 {}",
            template.transport,
            TRANSPORTS.join(" / ")
        )));
    }
    if template.server.trim().is_empty() {
        return Err(err("模板缺少服务器地址"));
    }
    if template.port == 0 {
        return Err(err("端口必须在 1–65535 之间"));
    }
    if template.name.trim().is_empty() {
        template.name = format!("{} · {}", template.kind.to_ascii_uppercase(), template.server);
    }
    Ok(template)
}

/// 标准与 URL-safe 两种字母表都接受，允许省略 padding。
fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for ch in input.chars() {
        if ch.is_whitespace() || ch == '=' {
            continue;
        }
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return Err(()),
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn vless_ws_tls_链接解析出全部字段() {
        let link = format!(
            "vless://{UUID}@cf.example.com:443?encryption=none&security=tls&sni=cdn.example.com&type=ws&host=cdn.example.com&path=%2Fws%3Fed%3D2048#%E9%A6%99%E6%B8%AF%E8%8A%82%E7%82%B9"
        );

        let template = parse_link(&link).unwrap();

        assert_eq!(template.kind, "vless");
        assert_eq!(template.uuid, UUID);
        assert_eq!(template.server, "cf.example.com");
        assert_eq!(template.port, 443);
        assert_eq!(template.transport, "ws");
        assert!(template.tls);
        assert_eq!(template.sni, "cdn.example.com");
        assert_eq!(template.host, "cdn.example.com");
        assert_eq!(template.path, "/ws?ed=2048");
        assert_eq!(template.name, "香港节点");
        assert!(template.service_name.is_empty());
    }

    #[test]
    fn vmess_grpc_链接解析出凭据与_service_name() {
        let link = concat!(
            "vmess://eyJ2IjoiMiIsInBzIjoiVk1lc3MtZ1JQQyIsImFkZCI6InZtLmV4YW1wbGUuY29tIiwicG9ydCI6Ijg0NDMiLCJpZCI6IjU1MGU4NDAwLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMCIsImFpZCI6IjAiLCJzY3kiOiJhdXRvIiwibmV0IjoiZ3JwYyIsInR5cGUiOiJub25lIiwiaG9zdCI6IiIsInBhdGgiOiJteXNlcnZpY2UiLCJ0bHMiOiJ0bHMiLCJzbmkiOiJ2bS5leGFtcGxlLmNvbSJ9"
        );

        let template = parse_link(link).unwrap();

        assert_eq!(template.kind, "vmess");
        assert_eq!(template.name, "VMess-gRPC");
        assert_eq!(template.uuid, UUID);
        assert_eq!(template.server, "vm.example.com");
        assert_eq!(template.port, 8443);
        assert_eq!(template.transport, "grpc");
        assert_eq!(template.service_name, "myservice");
        assert!(template.tls);
        assert_eq!(template.sni, "vm.example.com");
    }

    #[test]
    fn trojan_无_tls_链接解析正确() {
        let template =
            parse_link("trojan://pass%40word@t.example.com:8080?security=none&type=ws&path=%2Ft#Trojan-WS")
                .unwrap();

        assert_eq!(template.kind, "trojan");
        assert_eq!(template.password, "pass@word");
        assert_eq!(template.server, "t.example.com");
        assert_eq!(template.port, 8080);
        assert!(!template.tls);
        assert_eq!(template.transport, "ws");
        assert_eq!(template.path, "/t");
        assert_eq!(template.name, "Trojan-WS");
        assert!(template.uuid.is_empty());
    }

    #[test]
    fn ss_的_base64_变体都能解析() {
        // SIP002：base64 只包住 method:password。
        let sip002 =
            parse_link("ss://YWVzLTI1Ni1nY206c3NwYXNz@ss.example.com:8388#SS%E8%8A%82%E7%82%B9")
                .unwrap();
        assert_eq!(sip002.kind, "ss");
        assert_eq!(sip002.method, "aes-256-gcm");
        assert_eq!(sip002.password, "sspass");
        assert_eq!(sip002.server, "ss.example.com");
        assert_eq!(sip002.port, 8388);
        assert_eq!(sip002.transport, "tcp");
        assert!(!sip002.tls);
        assert_eq!(sip002.name, "SS节点");

        // 旧格式：整段 method:password@host:port 一起 base64。
        let legacy = parse_link(
            "ss://YWVzLTEyOC1nY206cEBzczp3b3JkQGxlZ2FjeS5leGFtcGxlLmNvbTo4Mzg4#Legacy",
        )
        .unwrap();
        assert_eq!(legacy.method, "aes-128-gcm");
        assert_eq!(legacy.password, "p@ss:word");
        assert_eq!(legacy.server, "legacy.example.com");
        assert_eq!(legacy.port, 8388);
        assert_eq!(legacy.name, "Legacy");
    }

    #[test]
    fn 非法链接给出可读错误且不产生模板() {
        let cases = [
            ("", "链接为空"),
            ("http://example.com", "不支持"),
            ("vless://host.example.com:443", "用户 ID"),
            ("vless://@host.example.com:443", "用户 ID"),
            ("vless://abc@host.example.com", "端口"),
            ("vless://abc@host.example.com:70000", "端口"),
            ("trojan://@host.example.com:443", "密码"),
            ("ss://不是base64", "base64"),
            ("vmess://abc", "vmess"),
            ("vmess://e30=", "服务器"),
            ("vless://abc@host.example.com:443?type=quic", "传输"),
        ];

        for (link, keyword) in cases {
            let error = parse_link(link).expect_err(link);
            assert!(
                error.0.contains(keyword),
                "链接 {link} 的错误「{}」应包含「{keyword}」",
                error.0
            );
        }
    }

    #[test]
    fn 未知协议与缺失协议前缀分别报错() {
        assert!(parse_link("example.com:443").unwrap_err().0.contains("协议"));
        assert!(parse_link("hysteria2://a@b:443").unwrap_err().0.contains("vless"));
    }

    #[test]
    fn ipv6_地址与查询参数大小写都能解析() {
        let template = parse_link(
            "vless://abc@[2001:db8::1]:443?Security=TLS&Type=GRPC&ServiceName=svc#IPv6",
        )
        .unwrap();

        assert_eq!(template.server, "2001:db8::1");
        assert_eq!(template.port, 443);
        assert_eq!(template.transport, "grpc");
        assert_eq!(template.service_name, "svc");
        assert!(template.tls);
    }

    #[test]
    fn 解析结果字段都在允许集合内() {
        let template = parse_link(&format!("vless://{UUID}@a.example.com:443?type=ws")).unwrap();
        assert!(KINDS.contains(&template.kind.as_str()));
        assert!(TRANSPORTS.contains(&template.transport.as_str()));
        assert!(template.name.contains("a.example.com"));
    }
}
