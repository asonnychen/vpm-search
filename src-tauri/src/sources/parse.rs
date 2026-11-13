//! 优选源文本与 bestcf 页面的纯解析逻辑。
//!
//! 这里只做字符串到结构的转换，不碰网络与数据库，方便单测覆盖边界。

/// 一条从优选源里解析出的优选地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    pub host: String,
    pub port: u16,
    /// `#` 之后的原始备注，原样保留（包括 `|` 与空格）。
    pub alias: String,
}

/// 一次文本解析的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParseOutcome {
    pub entries: Vec<ParsedEntry>,
    /// 无法识别的行数（空行与注释行不算）。
    pub skipped: usize,
    /// 因文件名里的 `topN` 被截掉的条数。
    pub truncated: usize,
}

/// 解析一份 `host:port#备注` 文本。
///
/// - 空行与 `#` / `//` / `;` 开头的注释行静默跳过；
/// - 无法解析的行计入 `skipped`；
/// - 文件名里带 `topN` 时只保留前 N 条，多出的计入 `truncated`。
pub fn parse_txt(text: &str, url: &str) -> ParseOutcome {
    let mut outcome = ParseOutcome::default();

    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() || is_comment(line) {
            continue;
        }
        match parse_line(line) {
            Some(entry) => outcome.entries.push(entry),
            None => outcome.skipped += 1,
        }
    }

    if let Some(limit) = top_limit(url) {
        if outcome.entries.len() > limit {
            outcome.truncated = outcome.entries.len() - limit;
            outcome.entries.truncate(limit);
        }
    }

    outcome
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with("//") || line.starts_with(';')
}

fn parse_line(line: &str) -> Option<ParsedEntry> {
    let (address, alias) = match line.split_once('#') {
        Some((address, alias)) => (address.trim(), alias.trim().to_string()),
        None => (line, String::new()),
    };

    let (host, port) = address.rsplit_once(':')?;
    let host = host.trim();
    if !is_valid_host(host) {
        return None;
    }
    let port: u16 = port.trim().parse().ok()?;
    if port == 0 {
        return None;
    }

    Some(ParsedEntry {
        host: host.to_ascii_lowercase(),
        port,
        alias,
    })
}

/// host 只允许主机名与 IPv4 的字符，避免把半截 URL 当成地址。
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// 从文件名里的 `topN` 取截断条数，例如 `top10.txt`、`top20-8443.txt`。
pub fn top_limit(url: &str) -> Option<usize> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let rest = file.split_once("top")?.1;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let limit: usize = digits.parse().ok()?;
    (limit > 0).then_some(limit)
}

/// bestcf 页面上「优选域名」区里的一条优选源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSource {
    pub url: String,
    /// 来源标识，形如 `BestCF·vps789/top10.txt`。
    pub name: String,
    /// 页面标注「更新异常」的源。
    pub broken: bool,
}

/// 待落入池的一条地址，带来源标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolCandidate {
    pub host: String,
    pub port: u16,
    pub alias: String,
    /// 来源标识，形如 `BestCF·vps789/top10.txt` 或 `手动`。
    pub source: String,
    /// 对应的优选源地址；手动粘贴的地址没有来源地址。
    pub source_url: Option<String>,
}

/// 按 `host:port` 去重，保留首次出现的那条，返回去重后的列表与被丢掉的条数。
pub fn dedupe(candidates: Vec<PoolCandidate>) -> (Vec<PoolCandidate>, usize) {
    let mut seen = std::collections::HashSet::new();
    let total = candidates.len();
    let kept: Vec<PoolCandidate> = candidates
        .into_iter()
        .filter(|c| seen.insert((c.host.to_ascii_lowercase(), c.port)))
        .collect();
    let dropped = total - kept.len();
    (kept, dropped)
}

/// 来源标识：bestcf 的源用 `BestCF·<路径>`，其它手动源用 `host<路径>`。
pub fn source_label(url: &str) -> String {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let (host, path) = match without_scheme.find('/') {
        Some(index) => (&without_scheme[..index], &without_scheme[index..]),
        None => (without_scheme, ""),
    };
    if host.eq_ignore_ascii_case("bestcf.pages.dev") {
        format!("BestCF·{}", path.trim_start_matches('/'))
    } else if path.is_empty() {
        host.to_string()
    } else {
        format!("{host}{path}")
    }
}

/// 从 bestcf 页面解析「优选域名」区（`id="yxym"`）的全部 txt 源。
///
/// 页面把失效源渲染成 `<del>` 或 `line-through`，同组内的所有链接都按异常处理。
pub fn parse_catalog(html: &str) -> Vec<CatalogSource> {
    let cleaned = strip_comments(html);
    let Some(section) = extract_section(&cleaned, "yxym") else {
        return Vec::new();
    };

    let mut sources = Vec::new();
    for block in section.split("<div class=\"group-card\"").skip(1) {
        let broken = block.contains("<del")
            || block.contains("line-through")
            || block.contains("更新异常");
        for url in urls_in(block) {
            if !url.ends_with(".txt") {
                continue;
            }
            if sources.iter().any(|s: &CatalogSource| s.url == url) {
                continue;
            }
            sources.push(CatalogSource {
                name: source_label(&url),
                url,
                broken,
            });
        }
    }
    sources
}

/// 去掉 HTML 注释：页面里被注释掉的源不应出现在清单中。
fn strip_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// 截取 `<section id="...">` 到它自己的 `</section>` 之间的内容。
fn extract_section<'a>(html: &'a str, id: &str) -> Option<&'a str> {
    let marker = format!("<section id=\"{id}\"");
    let start = html.find(&marker)?;
    let rest = &html[start..];
    let end = rest.find("</section>").unwrap_or(rest.len());
    Some(&rest[..end])
}

/// 页面把链接放在 `<span class="url-text">…</span>` 里。
fn urls_in(block: &str) -> Vec<String> {
    const MARKER: &str = "url-text\">";
    let mut urls = Vec::new();
    let mut rest = block;
    while let Some(index) = rest.find(MARKER) {
        let after = &rest[index + MARKER.len()..];
        let end = after.find('<').unwrap_or(after.len());
        let url = after[..end].trim();
        if !url.is_empty() {
            urls.push(url.to_string());
        }
        rest = &after[end..];
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top10_只保留前十条并丢掉广告行() {        let mut text = String::new();
        for i in 1..=10 {
            text.push_str(&format!("1.1.1.{i}:443#第 {i} 条\n"));
        }
        // 末尾的广告行同样能解析成地址，必须靠 topN 截断丢掉。
        text.push_str("9.9.9.9:443#分享免费优选网\n");

        let outcome = parse_txt(&text, "https://bestcf.pages.dev/vps789/top10.txt");

        assert_eq!(outcome.entries.len(), 10);
        assert_eq!(outcome.truncated, 1);
        assert_eq!(outcome.entries[9].host, "1.1.1.10");
    }

    #[test]
    fn 别名里的竖线原样保留() {
        let outcome = parse_txt(
            "162.159.198.1:8443#EDT导航优选域名 | BestCF.pages.dev\n",
            "https://bestcf.pages.dev/domain/all.txt",
        );

        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(
            outcome.entries[0].alias,
            "EDT导航优选域名 | BestCF.pages.dev"
        );
    }

    #[test]
    fn 空行与注释行被跳过且不计数() {
        let outcome = parse_txt(
            "\n   \n# 这是注释\n// 这也是注释\n; 还是注释\n1.1.1.1:443\n",
            "https://bestcf.pages.dev/domain/all.txt",
        );

        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(outcome.skipped, 0);
        assert_eq!(outcome.entries[0].alias, "");
    }

    #[test]
    fn 非法行被跳过并计数() {
        let outcome = parse_txt(
            "1.1.1.1:443\n没有端口\n1.1.1.2:0\n1.1.1.3:99999\n:443\n1.1.1.4:abc\n",
            "https://bestcf.pages.dev/domain/all.txt",
        );

        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(outcome.entries[0].host, "1.1.1.1");
        assert_eq!(outcome.skipped, 5);
    }

    fn candidate(host: &str, port: u16, alias: &str, source: &str) -> PoolCandidate {
        PoolCandidate {
            host: host.to_string(),
            port,
            alias: alias.to_string(),
            source: source.to_string(),
            source_url: None,
        }
    }

    #[test]
    fn 重复地址按_host_port_去重() {
        let (kept, dropped) = dedupe(vec![
            candidate("a.example.com", 443, "第一次", "源一"),
            candidate("a.example.com", 443, "第二次", "源二"),
            candidate("b.example.com", 443, "", "源一"),
        ]);

        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 1);
        assert_eq!(kept[0].alias, "第一次");
    }

    #[test]
    fn 同域名不同端口算两条() {
        let (kept, dropped) = dedupe(vec![
            candidate("a.example.com", 443, "", "源一"),
            candidate("a.example.com", 8443, "", "源一"),
        ]);

        assert_eq!(kept.len(), 2);
        assert_eq!(dropped, 0);
        assert_eq!(kept[1].port, 8443);
    }

    #[test]
    fn 大小写不同的同一域名只留一条() {
        let (kept, _) = dedupe(vec![
            candidate("a.example.com", 443, "", "源一"),
            candidate("A.Example.com", 443, "", "源二"),
        ]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn 来源标识按_bestcf_路径生成() {
        assert_eq!(
            source_label("https://bestcf.pages.dev/vps789/top10.txt"),
            "BestCF·vps789/top10.txt"
        );
        assert_eq!(
            source_label("https://example.com/all.txt"),
            "example.com/all.txt"
        );
    }

    const PAGE: &str = r#"
<section id="yxym">
  <div class="group-card">
    <div class="group-name">精选域名</div>
    <div class="link-row"><span class="url-text">https://bestcf.pages.dev/domain/all.txt</span></div>
    <div class="link-row"><span class="url-text">https://bestcf.pages.dev/domain/mini.txt</span></div>
  </div>
  <div class="group-card">
    <div class="group-name"><del>LuoLi</del><span class="tag">更新异常</span></div>
    <div class="action-area"><span class="url-text">https://bestcf.pages.dev/luoli/all.txt</span></div>
  </div>
  <!-- <div class="group-card"><span class="url-text">https://bestcf.pages.dev/wuya/all.txt</span></div> -->
</section>
<section id="yxip">
  <div class="group-card"><span class="url-text">https://bestcf.pages.dev/ip/all.txt</span></div>
</section>
"#;

    #[test]
    fn 页面只解析优选域名区并识别异常源() {
        let sources = parse_catalog(PAGE);

        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].url, "https://bestcf.pages.dev/domain/all.txt");
        assert_eq!(sources[0].name, "BestCF·domain/all.txt");
        assert!(!sources[0].broken);
        assert!(sources[2].broken);
        // 优选 IP 区与注释掉的源不应混进来。
        assert!(sources.iter().all(|s| !s.url.contains("/ip/")));
        assert!(sources.iter().all(|s| !s.url.contains("/wuya/")));
    }

    #[test]
    fn 页面结构缺失时返回空清单() {
        assert!(parse_catalog("<html><body>没有目标区块</body></html>").is_empty());
    }
}
