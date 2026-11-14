//! 节点模板：一条完整的代理配置。
//!
//! 模板保存协议类型、凭据、传输、TLS、SNI、host、path 等字段，
//! 测试时只替换其中的 server 与 port（阶段 6 起使用）。
//! 凭据只允许出现在设置库里，界面列表与日志一律显示 `****`。

pub mod parse;

use serde::{Deserialize, Serialize};

pub use parse::parse_link;

/// 凭据脱敏后的占位符，与日志脱敏保持一致。
pub const MASK: &str = "****";

/// 支持的协议类型。
pub const KINDS: [&str; 4] = ["vless", "vmess", "trojan", "ss"];

/// 支持的传输方式。
pub const TRANSPORTS: [&str; 5] = ["ws", "grpc", "h2", "tcp", "http"];

/// 一条节点模板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTemplate {
    pub id: String,
    pub name: String,
    /// 协议类型：vless / vmess / trojan / ss。
    pub kind: String,
    /// 模板里的原始 server，测试时会被优选地址替换。
    #[serde(default)]
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// vless / vmess 的用户 ID。
    #[serde(default)]
    pub uuid: String,
    /// trojan / ss 的密码。
    #[serde(default)]
    pub password: String,
    /// ss 的加密方式，例如 aes-256-gcm。
    #[serde(default)]
    pub method: String,
    /// 传输：ws / grpc / h2 / tcp / http。
    pub transport: String,
    pub tls: bool,
    #[serde(default)]
    pub sni: String,
    /// ws / h2 的 Host 头，vmess 里也可能用来指定 SNI。
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub path: String,
    /// grpc 的 serviceName。
    #[serde(default)]
    pub service_name: String,
}

fn default_port() -> u16 {
    443
}

impl Default for NodeTemplate {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            name: "我的 VLESS-WS".to_string(),
            kind: "vless".to_string(),
            server: String::new(),
            port: 443,
            uuid: String::new(),
            password: String::new(),
            method: String::new(),
            transport: "ws".to_string(),
            tls: true,
            sni: String::new(),
            host: String::new(),
            path: "/".to_string(),
            service_name: String::new(),
        }
    }
}

impl NodeTemplate {
    /// 测试前置校验：补齐可回退字段，并检查测试必需的凭据。
    ///
    /// - `ws` 传输的 `path` 为空时回退成 `/`（worker 只认根路径，空值会被直接拒绝）；
    /// - vless / vmess 必须有 `uuid`，trojan / ss 必须有 `password`；
    /// - 不通过时返回可直接展示的中文原因，调用方据此不启动 sing-box。
    pub fn prepare_for_probe(&mut self) -> Result<(), TemplateError> {
        if self.transport.trim().eq_ignore_ascii_case("ws") && self.path.trim().is_empty() {
            self.path = "/".to_string();
        }

        let kind = self.kind.trim();
        let missing = if kind.eq_ignore_ascii_case("vless") || kind.eq_ignore_ascii_case("vmess") {
            self.uuid.trim().is_empty().then_some("UUID")
        } else if kind.eq_ignore_ascii_case("trojan") || kind.eq_ignore_ascii_case("ss") {
            self.password.trim().is_empty().then_some("密码")
        } else {
            None
        };

        match missing {
            Some(field) => Err(TemplateError(format!(
                "当前节点模板「{}」缺少 {field}，请先粘贴节点链接或补全字段",
                self.name
            ))),
            None => Ok(()),
        }
    }

    /// 用于界面列表与日志的副本：凭据替换成 `****`，其余字段原样。
    pub fn redacted(&self) -> Self {
        let mut copy = self.clone();
        copy.uuid = mask_secret(&self.uuid);
        copy.password = mask_secret(&self.password);
        copy
    }

    /// 脱敏后的一行摘要，日志与列表共用。
    pub fn summary(&self) -> String {
        let redacted = self.redacted();
        let mut parts = vec![
            redacted.kind.to_ascii_uppercase(),
            redacted.transport.to_ascii_uppercase(),
        ];
        if redacted.tls {
            parts.push("TLS".to_string());
        }
        if !redacted.uuid.is_empty() {
            parts.push(format!("uuid {}", redacted.uuid));
        }
        if !redacted.password.is_empty() {
            parts.push(format!("password {}", redacted.password));
        }
        format!("{}（{}）", redacted.name, parts.join(" · "))
    }
}

/// 凭据脱敏：空值保持为空，非空一律 `****`。
pub fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        MASK.to_string()
    }
}

/// 模板校验错误，`Display` 给出可直接展示的中文原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError(pub String);

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TemplateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> NodeTemplate {
        NodeTemplate {
            id: "t1".to_string(),
            name: "测试节点".to_string(),
            kind: "vless".to_string(),
            server: "cf.example.com".to_string(),
            port: 443,
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            password: "hunter2".to_string(),
            method: String::new(),
            transport: "ws".to_string(),
            tls: true,
            sni: "cdn.example.com".to_string(),
            host: "cdn.example.com".to_string(),
            path: "/ws".to_string(),
            service_name: String::new(),
        }
    }

    #[test]
    fn 脱敏只替换凭据且不改变原文() {
        let original = template();
        let redacted = original.redacted();

        assert_eq!(redacted.uuid, MASK);
        assert_eq!(redacted.password, MASK);
        // 原文必须原样保留，界面拿到的是副本。
        assert_eq!(original.uuid, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(original.password, "hunter2");
        // 非凭据字段不受影响。
        assert_eq!(redacted.server, original.server);
        assert_eq!(redacted.sni, original.sni);
        assert_eq!(redacted.path, original.path);
    }

    #[test]
    fn 空凭据脱敏后仍为空() {
        let mut value = template();
        value.uuid.clear();
        value.password.clear();
        let redacted = value.redacted();
        assert_eq!(redacted.uuid, "");
        assert_eq!(redacted.password, "");
    }

    #[test]
    fn 摘要里不出现原始凭据() {
        let summary = template().summary();
        assert!(!summary.contains("550e8400"));
        assert!(!summary.contains("hunter2"));
        assert!(summary.contains(MASK));
    }

    #[test]
    fn vless_缺_uuid_时校验不通过并给出模板名() {
        let mut value = template();
        value.uuid.clear();

        let error = value.prepare_for_probe().unwrap_err();

        assert_eq!(
            error.to_string(),
            "当前节点模板「测试节点」缺少 UUID，请先粘贴节点链接或补全字段"
        );
    }

    #[test]
    fn vmess_必须有_uuid_而_trojan_与_ss_必须有密码() {
        let mut vmess = template();
        vmess.kind = "vmess".to_string();
        vmess.uuid.clear();
        assert!(vmess.prepare_for_probe().is_err());

        let mut trojan = template();
        trojan.kind = "trojan".to_string();
        trojan.password.clear();
        let error = trojan.prepare_for_probe().unwrap_err();
        assert!(error.to_string().contains("缺少 密码"), "{error}");

        let mut ss = template();
        ss.kind = "ss".to_string();
        ss.password = "  ".to_string();
        assert!(ss.prepare_for_probe().is_err());

        // 凭据齐全时通过。
        assert!(template().prepare_for_probe().is_ok());
    }

    #[test]
    fn ws_的空_path_回退成根路径() {
        let mut value = template();
        value.path = "   ".to_string();

        value.prepare_for_probe().unwrap();

        assert_eq!(value.path, "/");
        // 非 ws 传输不强制 path。
        let mut grpc = template();
        grpc.transport = "grpc".to_string();
        grpc.path.clear();
        grpc.prepare_for_probe().unwrap();
        assert_eq!(grpc.path, "");
    }
}
