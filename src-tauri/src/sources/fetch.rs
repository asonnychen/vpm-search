//! 网络抓取与多源合并。
//!
//! 所有请求显式 `no_proxy()`（见 ADR 0004），单源超时 10 秒；
//! 单源失败只影响该源，合并逻辑保持为纯函数以便单测。

use std::time::Duration;

use crate::sources::parse::{dedupe, parse_txt, CatalogSource, PoolCandidate};

/// bestcf 页面地址：优选源清单的唯一来源。
pub const CATALOG_URL: &str = "https://bestcf.pages.dev/";

/// 单个源的抓取超时。
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);

/// 手动粘贴地址的来源标识。
pub const MANUAL_SOURCE: &str = "手动";

/// 一个源本次抓取的结果，成功与失败都记在同一条里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedSource {
    pub url: String,
    pub name: String,
    /// 成功时的响应正文。
    pub body: Option<String>,
    /// 失败时的可读原因。
    pub error: Option<String>,
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
    pub entry_count: u32,
    pub error: Option<String>,
}

/// 把各源的抓取结果与手动地址合并成最终池内容（按 `host:port` 去重）。
pub fn assemble(sources: &[FetchedSource], manual_text: &str) -> PoolBuild {
    let mut build = PoolBuild::default();

    for source in sources {
        if let Some(error) = &source.error {
            build.outcomes.push(SourceOutcome {
                url: source.url.clone(),
                entry_count: 0,
                error: Some(error.clone()),
            });
            continue;
        }
        let body = source.body.as_deref().unwrap_or("");
        let outcome = parse_txt(body, &source.url);
        build.skipped += outcome.skipped;
        build.outcomes.push(SourceOutcome {
            url: source.url.clone(),
            entry_count: outcome.entries.len() as u32,
            error: None,
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

    // 手动地址放在最后，重复时保留抓取到的那条；换行与分号都算分隔符。
    let manual_text = manual_text.replace(';', "\n");
    let manual = parse_txt(&manual_text, "");
    build.skipped += manual.skipped;
    build
        .candidates
        .extend(manual.entries.into_iter().map(|entry| PoolCandidate {
            host: entry.host,
            port: entry.port,
            alias: entry.alias,
            source: MANUAL_SOURCE.to_string(),
            source_url: None,
        }));

    let (candidates, duplicates) = dedupe(build.candidates);
    build.candidates = candidates;
    build.duplicates = duplicates;
    build
}

/// 按 URL 抓取一个 txt 源，失败时返回可读原因。
pub async fn fetch_source(client: &reqwest::Client, source: &CatalogSource) -> FetchedSource {
    match fetch_text(client, &source.url).await {
        Ok(body) => FetchedSource {
            url: source.url.clone(),
            name: source.name.clone(),
            body: Some(body),
            error: None,
        },
        Err(error) => FetchedSource {
            url: source.url.clone(),
            name: source.name.clone(),
            body: None,
            error: Some(error),
        },
    }
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
        }
    }

    #[test]
    fn 单个源失败不影响其它源() {
        let sources = vec![
            FetchedSource {
                url: "https://bestcf.pages.dev/luoli/all.txt".to_string(),
                name: "BestCF·luoli/all.txt".to_string(),
                body: None,
                error: Some("HTTP 404".to_string()),
            },
            ok_source(
                "https://bestcf.pages.dev/domain/all.txt",
                "BestCF·domain/all.txt",
                "1.1.1.1:443#一\n2.2.2.2:8443#二\n",
            ),
        ];

        let build = assemble(&sources, "");

        assert_eq!(build.candidates.len(), 2);
        assert_eq!(build.outcomes[0].error.as_deref(), Some("HTTP 404"));
        assert_eq!(build.outcomes[0].entry_count, 0);
        assert_eq!(build.outcomes[1].entry_count, 2);
    }

    #[test]
    fn 手动地址与抓取结果合并去重() {
        let sources = vec![ok_source(
            "https://bestcf.pages.dev/domain/all.txt",
            "BestCF·domain/all.txt",
            "1.1.1.1:443#抓取\n2.2.2.2:443#抓取\n",
        )];
        // 换行与分号都能分隔；重复地址保留抓取到的那条。
        let manual = "2.2.2.2:443#手动重复\n3.3.3.3:443#手动 | 备注;4.4.4.4:8443#第二条";

        let build = assemble(&sources, manual);

        assert_eq!(build.candidates.len(), 4);
        assert_eq!(build.duplicates, 1);
        let duplicate = build
            .candidates
            .iter()
            .find(|c| c.host == "2.2.2.2")
            .unwrap();
        assert_eq!(duplicate.alias, "抓取");
        assert_eq!(duplicate.source, "BestCF·domain/all.txt");

        let manual_entry = build
            .candidates
            .iter()
            .find(|c| c.host == "3.3.3.3")
            .unwrap();
        assert_eq!(manual_entry.source, MANUAL_SOURCE);
        assert_eq!(manual_entry.alias, "手动 | 备注");
    }

    #[test]
    fn 分号分隔的手动地址也能解析() {
        let build = assemble(&[], "1.1.1.1:443;2.2.2.2:443\n3.3.3.3:443");
        assert_eq!(build.candidates.len(), 3);
    }

    #[test]
    fn 解析失败的行计入跳过数() {
        let sources = vec![ok_source(
            "https://bestcf.pages.dev/domain/all.txt",
            "BestCF·domain/all.txt",
            "1.1.1.1:443\n坏行\n",
        )];
        let build = assemble(&sources, "也不是地址");
        assert_eq!(build.candidates.len(), 1);
        assert_eq!(build.skipped, 2);
    }
}
