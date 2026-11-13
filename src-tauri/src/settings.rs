//! 应用设置的领域类型与校验。
//!
//! 设置以单行 KV 的形式存进 `settings` 表：每个字段一个 key，值是该字段的 JSON。
//! 这样某个字段损坏或缺失时只会回落到该字段的默认值，不会让整份设置失效。

use serde::{Deserialize, Serialize};

pub use crate::template::NodeTemplate;

/// 表格列数，与 `ResultsTable` 的列一一对应。
pub const COLUMN_COUNT: usize = 14;

/// 表格列宽的默认值，与界面原型的初始列宽一致。
pub const DEFAULT_COLUMN_WIDTHS: [u32; COLUMN_COUNT] = [
    64, 60, 172, 140, 58, 54, 84, 96, 54, 46, 156, 112, 88, 84,
];

/// 三类日志的保留天数默认值。
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 30;

/// 请求总超时固定 10 秒，不随设置变化。
pub const FIXED_REQUEST_TIMEOUT_S: u32 = 10;

/// 每条地址的延迟采样固定 3 次，不随设置变化。
pub const FIXED_LATENCY_SAMPLES: u32 = 3;

fn default_request_timeout_s() -> u32 {
    FIXED_REQUEST_TIMEOUT_S
}

fn default_latency_samples() -> u32 {
    FIXED_LATENCY_SAMPLES
}

/// 设置里每个字段在 `settings` 表中的 key。
pub const SETTING_KEYS: [&str; 12] = [
    "last_input",
    "node_templates",
    "active_template_id",
    "max_latency_ms",
    "connect_timeout_s",
    "request_timeout_s",
    "concurrency",
    "latency_samples",
    "speed",
    "column_widths",
    "log_retention",
    "ui",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastInput {
    /// 优选源 txt 地址，每行一个。
    pub source_urls: String,
    /// 手动补充的优选地址，换行或分号分隔。
    pub manual_addresses: String,
}

impl Default for LastInput {
    fn default() -> Self {
        Self {
            source_urls: String::new(),
            manual_addresses: String::new(),
        }
    }
}

/// 测速参数：流数、下载数据量、总超时。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedParams {
    pub streams: u32,
    pub size_mb: u32,
    pub timeout_s: u32,
}

impl Default for SpeedParams {
    fn default() -> Self {
        Self {
            streams: 4,
            size_mb: 10,
            timeout_s: 15,
        }
    }
}

/// 三类日志各自独立的保留天数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRetention {
    pub operation_days: u32,
    pub app_days: u32,
    pub singbox_days: u32,
}

impl Default for LogRetention {
    fn default() -> Self {
        Self {
            operation_days: DEFAULT_LOG_RETENTION_DAYS,
            app_days: DEFAULT_LOG_RETENTION_DAYS,
            singbox_days: DEFAULT_LOG_RETENTION_DAYS,
        }
    }
}

/// 界面状态：日志面板高度与折叠状态，重启后还原。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub log_height: u32,
    pub log_collapsed: bool,
    pub config_open: bool,
    pub pool_open: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            log_height: 214,
            log_collapsed: false,
            config_open: true,
            pool_open: false,
        }
    }
}

/// 一份完整的应用设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub last_input: LastInput,
    pub node_templates: Vec<NodeTemplate>,
    /// 当前选中的模板 id；为空表示没有模板。
    pub active_template_id: String,
    /// 超阈值判定用的最大延时。
    pub max_latency_ms: u32,
    pub connect_timeout_s: u32,
    /// 保留字段：请求总超时已固定为 [`FIXED_REQUEST_TIMEOUT_S`]（10 秒），
    /// 后端不再读取该值；保留它只为兼容旧设置 JSON 与旧数据库行。
    #[serde(default = "default_request_timeout_s")]
    pub request_timeout_s: u32,
    pub concurrency: u32,
    /// 保留字段：延迟采样已固定为 [`FIXED_LATENCY_SAMPLES`]（3 次），
    /// 后端不再读取该值；保留它只为兼容旧设置 JSON 与旧数据库行。
    #[serde(default = "default_latency_samples")]
    pub latency_samples: u32,
    /// 保留字段：测速固定为 4 流 · 10MB · 15 秒（[`SpeedParams::default`]），
    /// 后端不再读取该值；保留它只为兼容旧设置 JSON 与旧数据库行。
    #[serde(default)]
    pub speed: SpeedParams,
    pub column_widths: Vec<u32>,
    pub log_retention: LogRetention,
    pub ui: UiState,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_input: LastInput::default(),
            node_templates: vec![NodeTemplate::default()],
            active_template_id: NodeTemplate::default().id,
            max_latency_ms: 200,
            connect_timeout_s: 5,
            request_timeout_s: 10,
            concurrency: 10,
            latency_samples: 3,
            speed: SpeedParams::default(),
            column_widths: DEFAULT_COLUMN_WIDTHS.to_vec(),
            log_retention: LogRetention::default(),
            ui: UiState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ValidationError {}

fn in_range(name: &str, value: u32, min: u32, max: u32) -> Result<(), ValidationError> {
    if value < min || value > max {
        return Err(ValidationError(format!(
            "{name} 必须在 {min}–{max} 之间，实际为 {value}"
        )));
    }
    Ok(())
}

impl Settings {
    /// 把已固定的只读参数钳回契约值：旧设置里即使存了别的值也不生效。
    pub fn normalize_reserved(&mut self) {
        self.request_timeout_s = FIXED_REQUEST_TIMEOUT_S;
        self.latency_samples = FIXED_LATENCY_SAMPLES;
        self.speed = SpeedParams::default();
    }

    /// 参数校验。命令层只做校验与转发，落库前必须通过这里。
    pub fn validate(&self) -> Result<(), ValidationError> {
        in_range("最大延时", self.max_latency_ms, 1, 10_000)?;
        in_range("连接超时", self.connect_timeout_s, 1, 10)?;
        in_range("请求总超时", self.request_timeout_s, 1, 120)?;
        in_range("地址并发", self.concurrency, 1, 32)?;
        in_range("延迟采样", self.latency_samples, 1, 10)?;
        in_range("测速流数", self.speed.streams, 1, 32)?;
        in_range("测速数据量", self.speed.size_mb, 1, 1024)?;
        in_range("测速超时", self.speed.timeout_s, 1, 300)?;
        in_range(
            "操作日志保留天数",
            self.log_retention.operation_days,
            1,
            3650,
        )?;
        in_range("应用日志保留天数", self.log_retention.app_days, 1, 3650)?;
        in_range(
            "sing-box 日志保留天数",
            self.log_retention.singbox_days,
            1,
            3650,
        )?;
        in_range("日志面板高度", self.ui.log_height, 80, 4000)?;

        if self.column_widths.len() != COLUMN_COUNT {
            return Err(ValidationError(format!(
                "表格列宽必须是 {COLUMN_COUNT} 项，实际为 {}",
                self.column_widths.len()
            )));
        }
        for (i, width) in self.column_widths.iter().enumerate() {
            if *width < 40 || *width > 2000 {
                return Err(ValidationError(format!(
                    "第 {} 列列宽必须在 40–2000 之间，实际为 {width}",
                    i + 1
                )));
            }
        }

        for template in &self.node_templates {
            if template.id.trim().is_empty() {
                return Err(ValidationError("节点模板 id 不能为空".to_string()));
            }
            if template.name.trim().is_empty() {
                return Err(ValidationError("节点模板名称不能为空".to_string()));
            }
            if !crate::template::KINDS.contains(&template.kind.as_str()) {
                return Err(ValidationError(format!(
                    "节点模板「{}」的协议类型「{}」不支持",
                    template.name, template.kind
                )));
            }
            if !crate::template::TRANSPORTS.contains(&template.transport.as_str()) {
                return Err(ValidationError(format!(
                    "节点模板「{}」的传输方式「{}」不支持",
                    template.name, template.transport
                )));
            }
            if template.port == 0 {
                return Err(ValidationError(format!(
                    "节点模板「{}」的端口必须在 1–65535 之间",
                    template.name
                )));
            }
            if self
                .node_templates
                .iter()
                .filter(|other| other.id == template.id)
                .count()
                > 1
            {
                return Err(ValidationError(format!(
                    "节点模板 id「{}」重复",
                    template.id
                )));
            }
        }

        if !self.active_template_id.is_empty()
            && !self
                .node_templates
                .iter()
                .any(|template| template.id == self.active_template_id)
        {
            return Err(ValidationError(format!(
                "当前节点模板「{}」不在模板列表中",
                self.active_template_id
            )));
        }

        if self.last_input.source_urls.chars().count() > 64 * 1024
            || self.last_input.manual_addresses.chars().count() > 64 * 1024
        {
            return Err(ValidationError("最后输入内容过长（上限 64KB）".to_string()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认设置通过校验() {
        Settings::default().validate().unwrap();
    }

    #[test]
    fn 并发超出范围会被拒绝() {
        let settings = Settings {
            concurrency: 33,
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn 列宽数量不对会被拒绝() {
        let settings = Settings {
            column_widths: vec![64, 60],
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn 节点模板名称不能为空() {
        let mut settings = Settings::default();
        settings.node_templates[0].name = "  ".to_string();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn 只读参数被钳回固定契约值() {
        let mut settings = Settings {
            request_timeout_s: 99,
            latency_samples: 9,
            speed: SpeedParams {
                streams: 16,
                size_mb: 512,
                timeout_s: 300,
            },
            ..Settings::default()
        };
        settings.normalize_reserved();
        assert_eq!(settings.request_timeout_s, FIXED_REQUEST_TIMEOUT_S);
        assert_eq!(settings.latency_samples, FIXED_LATENCY_SAMPLES);
        assert_eq!(settings.speed, SpeedParams::default());
        settings.validate().unwrap();
    }

    #[test]
    fn 旧设置_json_缺少只读字段也能反序列化() {
        let json = serde_json::json!({
            "lastInput": { "sourceUrls": "", "manualAddresses": "" },
            "nodeTemplates": [],
            "activeTemplateId": "",
            "maxLatencyMs": 200,
            "connectTimeoutS": 5,
            "concurrency": 10,
            "columnWidths": DEFAULT_COLUMN_WIDTHS,
            "logRetention": { "operationDays": 30, "appDays": 30, "singboxDays": 30 },
            "ui": { "logHeight": 214, "logCollapsed": false, "configOpen": true, "poolOpen": false }
        });
        let settings: Settings = serde_json::from_value(json).expect("旧 JSON 应能解析");
        assert_eq!(settings.request_timeout_s, FIXED_REQUEST_TIMEOUT_S);
        assert_eq!(settings.latency_samples, FIXED_LATENCY_SAMPLES);
        assert_eq!(settings.speed, SpeedParams::default());
    }
}
