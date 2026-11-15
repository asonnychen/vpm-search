//! 网络抓取与多源合并。
//!
//! 所有请求显式 `no_proxy()`（见 ADR 0004），单源超时 10 秒；
//! 单源失败只影响该源，合并逻辑保持为纯函数以便单测。
//!
//! 抓取结果分三种：
//! - 有效源：解析出至少一条地址，进池；
//! - 暂时失败：超时、连接失败，保留在清单里并标 `lastError`，默认不勾选；
//! - 无效源：HTTP ≥ 400、内容不是合法 txt、解析为 0 条，直接丢弃。

use std::time::Duration;

use crate::sources::parse::{dedupe, parse_txt, CatalogSource, PoolCandidate};

/// bestcf 页面地址：优选源清单的唯一来源。
pub const CATALOG_URL: &str = "https://bestcf.pages.dev/";

/// 单个源的抓取超时。
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一个源本次抓取的结果，成功、失败与丢弃都记在同一条里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedSource {
    pub url: String,
    pub name: String,
    /// 成功时的响应正文。
    pub body: Option<String>,
    /// 暂时失败时的可读原因（保留源，默认不勾选）。
    pub error: Option<String>,
    /// 判定为无效源的原因；有值时主进程会从 `pool_source` 删除它。
    pub drop_reason: Option<String>,
    /// 本次抓取耗时（毫秒），写进应用日志。
    pub duration_ms: u64,
}

/// 合并后的池内容与每个源的抓取结论。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PoolBuild {
    pub candidates: Vec<PoolCandidate>,
    pub outcomes: Vec<SourceOutcome>,
    /// 被去重掉的重复地址条数。
    pub duplicates: usize,
    /// 所有源里无法解析的行数之和。
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOutcome {
    pub url: String,
    pub name: String,
    pub entry_count: u32,
    /// 暂时失败的原因；保留源并标记。
    pub error: Option<String>,
    /// 无效源的原因；丢弃源。
    pub drop_reason: Option<String>,
    pub duration_ms: u64,
}

/// 判断响应正文是不是一份合法的 txt 优选源。
pub fn is_valid_txt(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("内容为空".to_string());
    }
    if body.contains('\0') {
        return Err("内容包含二进制字符".to_string());
    }
    if body.contains('\u{FFFD}') {
        return Err("内容不是合法 UTF-8 文本".to_string());
    }
    let head: String = body
        .trim_start()
        .chars()
        .take(64)
        .collect::<String>()
        .to_lowercase();
    if head.starts_with("<!doctype")
        || head.starts_with("<html")
        || head.starts_with("<?xml")
        || head.starts_with("<head")
    {
        return Err("内容不是 txt".to_string());
    }
    Ok(())
}

/// 把各源的抓取结果合并成最终池内容（按 `host:port` 去重）。
pub fn assemble(sources: &[FetchedSource]) -> PoolBuild {
    let mut build = PoolBuild::default();

    for source in sources {
        if let Some(reason) = &source.drop_reason {
            build.outcomes.push(SourceOutcome {
                url: source.url.clone(),
                name: source.name.clone(),
                entry_count: 0,
                error: None,
                drop_reason: Some(reason.clone()),
                duration_ms: source.duration_ms,
            });
            continue;
        }
        if let Some(error) = &source.error {
            build.outcomes.push(SourceOutcome {
                url: source.url.clone(),
                name: source.name.clone(),
                entry_count: 0,
                error: Some(error.clone()),
                drop_reason: None,
                duration_ms: source.duration_ms,
            });
            continue;
        }
        let body = source.body.as_deref().unwrap_or("");
        let outcome = parse_txt(body, &source.url);
        build.skipped += outcome.skipped;
        if outcome.entries.is_empty() {
            build.outcomes.push(SourceOutcome {
                url: source.url.clone(),
                name: source.name.clone(),
                entry_count: 0,
                error: None,
                drop_reason: Some("解析结果为 0 条".to_string()),
                duration_ms: source.duration_ms,
            });
            continue;
        }
        build.outcomes.push(SourceOutcome {
            url: source.url.clone(),
            name: source.name.clone(),
            entry_count: outcome.entries.len() as u32,
            error: None,
            drop_reason: None,
            duration_ms: source.duration_ms,
        });
        build
            .candidates
            .extend(outcome.entries.into_iter().map(|entry| PoolCandidate {
                host: entry.host,
                port: entry.port,
                alias: entry.alias,
                source: source.name.clone(),
                source_url: Some(source.url.clone()),
            }));
    }

    let (candidates, duplicates) = dedupe(build.candidates);
    build.candidates = candidates;
    build.duplicates = duplicates;
    build
}

/// 按 URL 抓取一个 txt 源，失败时返回可读原因。
pub async fn fetch_source(client: &reqwest::Client, source: &CatalogSource) -> FetchedSource {
    let started = std::time::Instant::now();
    let response = client.get(&source.url).send().await;
    let duration_ms = started.elapsed().as_millis() as u64;

    let mut fetched = FetchedSource {
        url: source.url.clone(),
        name: source.name.clone(),
        body: None,
        error: None,
        drop_reason: None,
        duration_ms,
    };

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // 超时与连接失败只算「暂时失败」，源保留在清单里。
            fetched.error = Some(if error.is_timeout() {
                format!("请求超时（{}s）", SOURCE_TIMEOUT.as_secs())
            } else {
                format!("请求失败：{error}")
            });
            return fetched;
        }
    };

    if !response.status().is_success() {
        // HTTP ≥ 400 是确定的无效源，直接丢弃。
        fetched.drop_reason = Some(format!("HTTP {}", response.status().as_u16()));
        return fetched;
    }

    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            fetched.error = Some(format!("读取响应失败：{error}"));
            return fetched;
        }
    };

    match is_valid_txt(&body) {
        Ok(()) => fetched.body = Some(body),
        Err(reason) => fetched.drop_reason = Some(reason),
    }
    fetched
}

/// 并发抓取所有源；单源失败不影响其它源。
pub async fn fetch_all(
    client: &reqwest::Client,
    sources: &[CatalogSource],
) -> Vec<FetchedSource> {
    let tasks = sources.iter().map(|source| fetch_source(client, source));
    futures_util::future::join_all(tasks).await
}

/// 建立一个绕过系统代理、单请求 10 秒超时的客户端。
pub fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(SOURCE_TIMEOUT)
        .user_agent("vpm-search/0.1")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))
}

/// 取回一段文本，把底层错误翻译成界面上能读懂的话。
pub async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            format!("请求超时（{}s）", SOURCE_TIMEOUT.as_secs())
        } else {
            format!("请求失败：{e}")
        }
    })?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    response
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_source(url: &str, name: &str, body: &str) -> FetchedSource {
        FetchedSource {
            url: url.to_string(),
            name: name.to_string(),
            body: Some(body.to_string()),
            error: None,
            drop_reason: None,
            duration_ms: 12,
        }
    }

    #[test]
    fn 单个源失败不影响其它源() {
        let sources = vec![
            FetchedSource {
                url: "https://bestcf.pages.dev/luoli/all.txt".to_string(),
                name: "BestCF·luoli/all.txt".to_string(),
                body: None,
                error: Some("请求超时（10s）".to_string()),
                drop_reason: None,
                duration_ms: 34,
            },
            ok_source(
                "https://bestcf.pages.dev/domain/all.txt",
                "BestCF·domain/all.txt",
                "1.1.1.1:443#一\n2.2.2.2:8443#二\n",
            ),
        ];

        let build = assemble(&sources);

        assert_eq!(build.candidates.len(), 2);
        assert_eq!(build.outcomes[0].error.as_deref(), Some("请求超时（10s）"));
        assert_eq!(build.outcomes[0].drop_reason, None, "超时源要保留");
        assert_eq!(build.outcomes[0].entry_count, 0);
        assert_eq!(build.outcomes[1].entry_count, 2);
        assert_eq!(build.outcomes[1].duration_ms, 12);
    }

    #[test]
    fn http_404_的源被丢弃() {
        let sources = vec![FetchedSource {
            url: "https://bestcf.pages.dev/missing/all.txt".to_string(),
            name: "BestCF·missing/all.txt".to_string(),
            body: None,
            error: None,
            drop_reason: Some("HTTP 404".to_string()),
            duration_ms: 8,
        }];

        let build = assemble(&sources);
        assert!(build.candidates.is_empty());
        assert_eq!(build.outcomes[0].drop_reason.as_deref(), Some("HTTP 404"));
        assert_eq!(build.outcomes[0].error, None);
    }

    #[test]
    fn 空内容与解析为零条的源被丢弃() {
        // 抓取阶段判定空内容。
        let sources = vec![FetchedSource {
            url: "https://bestcf.pages.dev/empty/all.txt".to_string(),
            name: "BestCF·empty/all.txt".to_string(),
            body: None,
            error: None,
            drop_reason: Some("内容为空".to_string()),
            duration_ms: 4,
        }];
        let build = assemble(&sources);
        assert_eq!(build.outcomes[0].drop_reason.as_deref(), Some("内容为空"));

        // 有内容但一行地址都解析不出来。
        let sources = vec![ok_source(
            "https://bestcf.pages.dev/junk/all.txt",
            "BestCF·junk/all.txt",
            "这不是地址\n也不是地址\n",
        )];
        let build = assemble(&sources);
        assert!(build.candidates.is_empty());
        assert_eq!(
            build.outcomes[0].drop_reason.as_deref(),
            Some("解析结果为 0 条")
        );
        assert_eq!(build.skipped, 2);
    }

    #[test]
    fn 正常源不受过滤规则影响() {
        let sources = vec![ok_source(
            "https://bestcf.pages.dev/domain/all.txt",
            "BestCF·domain/all.txt",
            "1.1.1.1:443#一\n2.2.2.2:8443#二\n",
        )];
        let build = assemble(&sources);
        assert_eq!(build.outcomes[0].drop_reason, None);
        assert_eq!(build.outcomes[0].error, None);
        assert_eq!(build.candidates.len(), 2);
    }

    #[test]
    fn 合法_txt_判定() {
        assert!(is_valid_txt("1.1.1.1:443#一\n").is_ok());
        assert!(is_valid_txt("   ").is_err());
        assert!(is_valid_txt("<!DOCTYPE html><html></html>").is_err());
        assert!(is_valid_txt("a\0b").is_err());
        assert!(is_valid_txt("坏\u{FFFD}字符").is_err());
    }

    #[test]
    fn 重复地址只保留第一次出现() {
        let sources = vec![
            ok_source(
                "https://bestcf.pages.dev/a/all.txt",
                "BestCF·a/all.txt",
                "1.1.1.1:443#一\n",
            ),
            ok_source(
                "https://bestcf.pages.dev/b/all.txt",
                "BestCF·b/all.txt",
                "1.1.1.1:443#重复\n2.2.2.2:443#二\n",
            ),
        ];
        let build = assemble(&sources);
        assert_eq!(build.candidates.len(), 2);
        assert_eq!(build.duplicates, 1);
        assert_eq!(build.candidates[0].alias, "一");
    }
}
