//! 为一次探测生成临时 sing-box 配置。
//!
//! 一个 mixed 入站 + 一个对应协议的出站；只替换模板里的 server / port，
//! 其余字段（凭据、传输、TLS、SNI、path）原样保留。

use serde_json::{json, Map, Value};

use super::Endpoint;
use crate::template::NodeTemplate;

/// 本地入站监听地址，只绑回环，绝不对外暴露。
pub const LISTEN_HOST: &str = "127.0.0.1";

/// 生成完整配置。
pub fn build_config(endpoint: &Endpoint, template: &NodeTemplate, listen_port: u16) -> Value {
    json!({
        "log": { "level": "info", "timestamp": true },
        "inbounds": [{
            "type": "mixed",
            "tag": "in",
            "listen": LISTEN_HOST,
            "listen_port": listen_port,
        }],
        "outbounds": [build_outbound(endpoint, template)],
    })
}

/// 一个槽位进程的配置：每条地址一条 lane（mixed 入站 + 对应出站），
/// 用路由规则把入站流量固定送到自己的出站。
///
/// 这样每个槽位只启动一次进程，整轮里反复复用，不必每条地址启停。
pub fn build_slot_config(lanes: &[(Endpoint, u16)], template: &NodeTemplate) -> Value {
    let mut inbounds = Vec::with_capacity(lanes.len());
    let mut outbounds = Vec::with_capacity(lanes.len());
    let mut rules = Vec::with_capacity(lanes.len());

    for (index, (endpoint, listen_port)) in lanes.iter().enumerate() {
        let in_tag = format!("in-{index}");
        let out_tag = format!("out-{index}");
        inbounds.push(json!({
            "type": "mixed",
            "tag": in_tag,
            "listen": LISTEN_HOST,
            "listen_port": listen_port,
        }));
        outbounds.push(build_outbound_tagged(endpoint, template, &out_tag));
        rules.push(json!({
            "inbound": [in_tag],
            "outbound": out_tag,
        }));
    }

    json!({
        "log": { "level": "info", "timestamp": true },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": { "rules": rules },
    })
}

/// 生成一个出站，server 与 port 用优选地址替换。
pub fn build_outbound(endpoint: &Endpoint, template: &NodeTemplate) -> Value {
    build_outbound_tagged(endpoint, template, "proxy")
}

/// 生成一个带指定 tag 的出站。
pub fn build_outbound_tagged(endpoint: &Endpoint, template: &NodeTemplate, tag: &str) -> Value {
    let mut outbound = Map::new();
    outbound.insert("tag".to_string(), json!(tag));
    outbound.insert("server".to_string(), json!(endpoint.host));
    outbound.insert("server_port".to_string(), json!(endpoint.port));

    match template.kind.as_str() {
        "vless" => {
            outbound.insert("type".to_string(), json!("vless"));
            outbound.insert("uuid".to_string(), json!(template.uuid));
            outbound.insert("packet_encoding".to_string(), json!("xudp"));
        }
        "vmess" => {
            outbound.insert("type".to_string(), json!("vmess"));
            outbound.insert("uuid".to_string(), json!(template.uuid));
            outbound.insert("security".to_string(), json!("auto"));
            outbound.insert("alter_id".to_string(), json!(0));
        }
        "trojan" => {
            outbound.insert("type".to_string(), json!("trojan"));
            outbound.insert("password".to_string(), json!(template.password));
        }
        "ss" => {
            outbound.insert("type".to_string(), json!("shadowsocks"));
            outbound.insert("method".to_string(), json!(template.method));
            outbound.insert("password".to_string(), json!(template.password));
            // shadowsocks 没有 TLS 与传输层，保持最小配置。
            return Value::Object(outbound);
        }
        // 设置校验已挡住未知协议，这里兜底成一个不可用的出站而不是 panic。
        other => {
            outbound.insert("type".to_string(), json!(other));
        }
    }

    if template.tls {
        let server_name = if !template.sni.is_empty() {
            template.sni.clone()
        } else if !template.host.is_empty() {
            template.host.clone()
        } else {
            endpoint.host.clone()
        };
        outbound.insert(
            "tls".to_string(),
            json!({ "enabled": true, "server_name": server_name }),
        );
    }

    if let Some(transport) = build_transport(template) {
        outbound.insert("transport".to_string(), transport);
    }

    Value::Object(outbound)
}

/// 传输层配置；tcp 没有额外字段。
fn build_transport(template: &NodeTemplate) -> Option<Value> {
    match template.transport.as_str() {
        "ws" => {
            let mut ws = Map::new();
            ws.insert("type".to_string(), json!("ws"));
            ws.insert("path".to_string(), json!(template.path));
            if !template.host.is_empty() {
                ws.insert("headers".to_string(), json!({ "Host": template.host }));
            }
            Some(Value::Object(ws))
        }
        "grpc" => Some(json!({
            "type": "grpc",
            "service_name": template.service_name,
        })),
        "h2" | "http" => {
            let mut http = Map::new();
            http.insert("type".to_string(), json!("http"));
            if !template.host.is_empty() {
                http.insert("host".to_string(), json!([template.host]));
            }
            http.insert("path".to_string(), json!(template.path));
            Some(Value::Object(http))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint::new("104.16.1.1", 8443)
    }

    fn template(kind: &str, transport: &str) -> NodeTemplate {
        NodeTemplate {
            id: "t1".to_string(),
            name: "测试节点".to_string(),
            kind: kind.to_string(),
            server: "原服务器.example.com".to_string(),
            port: 443,
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            password: "hunter2".to_string(),
            method: "aes-256-gcm".to_string(),
            transport: transport.to_string(),
            tls: true,
            sni: "cdn.example.com".to_string(),
            host: "cdn.example.com".to_string(),
            path: "/ws".to_string(),
            service_name: "gsvc".to_string(),
        }
    }

    #[test]
    fn vless_ws_tls_只替换_server_与_port() {
        let config = build_config(&endpoint(), &template("vless", "ws"), 23456);
        let out = &config["outbounds"][0];

        assert_eq!(out["type"], "vless");
        assert_eq!(out["server"], "104.16.1.1");
        assert_eq!(out["server_port"], 8443);
        // 凭据与传输原样保留。
        assert_eq!(out["uuid"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(out["transport"]["type"], "ws");
        assert_eq!(out["transport"]["path"], "/ws");
        assert_eq!(out["transport"]["headers"]["Host"], "cdn.example.com");
        assert_eq!(out["tls"]["enabled"], true);
        assert_eq!(out["tls"]["server_name"], "cdn.example.com");

        // 本地 mixed 入站只绑回环。
        assert_eq!(config["inbounds"][0]["type"], "mixed");
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(config["inbounds"][0]["listen_port"], 23456);
    }

    #[test]
    fn vmess_grpc_保留_uuid_与_service_name() {
        let config = build_config(&endpoint(), &template("vmess", "grpc"), 20001);
        let out = &config["outbounds"][0];

        assert_eq!(out["type"], "vmess");
        assert_eq!(out["server"], "104.16.1.1");
        assert_eq!(out["server_port"], 8443);
        assert_eq!(out["uuid"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(out["security"], "auto");
        assert_eq!(out["transport"]["type"], "grpc");
        assert_eq!(out["transport"]["service_name"], "gsvc");
    }

    #[test]
    fn trojan_h2_保留密码与路径() {
        let config = build_config(&endpoint(), &template("trojan", "h2"), 20002);
        let out = &config["outbounds"][0];

        assert_eq!(out["type"], "trojan");
        assert_eq!(out["password"], "hunter2");
        assert_eq!(out["transport"]["type"], "http");
        assert_eq!(out["transport"]["path"], "/ws");
        assert_eq!(out["transport"]["host"][0], "cdn.example.com");
        assert_eq!(out["tls"]["server_name"], "cdn.example.com");
    }

    #[test]
    fn ss_保留加密方式且不带_tls() {
        let mut ss = template("ss", "tcp");
        ss.tls = false;
        let config = build_config(&endpoint(), &ss, 20003);
        let out = &config["outbounds"][0];

        assert_eq!(out["type"], "shadowsocks");
        assert_eq!(out["server"], "104.16.1.1");
        assert_eq!(out["server_port"], 8443);
        assert_eq!(out["method"], "aes-256-gcm");
        assert_eq!(out["password"], "hunter2");
        assert!(out.get("tls").is_none());
    }

    #[test]
    fn 没有_sni_时退回_host_再退回优选地址() {
        let mut value = template("vless", "tcp");
        value.sni.clear();
        let config = build_config(&endpoint(), &value, 20004);
        assert_eq!(config["outbounds"][0]["tls"]["server_name"], "cdn.example.com");

        value.host.clear();
        let config = build_config(&endpoint(), &value, 20004);
        assert_eq!(config["outbounds"][0]["tls"]["server_name"], "104.16.1.1");
    }

    #[test]
    fn 槽位配置为每条地址生成一条入站出站与路由() {
        let lanes = vec![
            (Endpoint::new("104.16.1.1", 8443), 20_000),
            (Endpoint::new("104.16.2.2", 2053), 20_001),
        ];
        let config = build_slot_config(&lanes, &template("vless", "ws"));

        assert_eq!(config["inbounds"].as_array().unwrap().len(), 2);
        assert_eq!(config["inbounds"][0]["tag"], "in-0");
        assert_eq!(config["inbounds"][0]["listen_port"], 20_000);
        assert_eq!(config["inbounds"][0]["listen"], "127.0.0.1");
        assert_eq!(config["inbounds"][1]["listen_port"], 20_001);

        assert_eq!(config["outbounds"].as_array().unwrap().len(), 2);
        assert_eq!(config["outbounds"][0]["tag"], "out-0");
        assert_eq!(config["outbounds"][0]["server"], "104.16.1.1");
        assert_eq!(config["outbounds"][1]["tag"], "out-1");
        assert_eq!(config["outbounds"][1]["server_port"], 2053);

        assert_eq!(config["route"]["rules"][0]["inbound"][0], "in-0");
        assert_eq!(config["route"]["rules"][0]["outbound"], "out-0");
        assert_eq!(config["route"]["rules"][1]["inbound"][0], "in-1");
        assert_eq!(config["route"]["rules"][1]["outbound"], "out-1");
    }
}
