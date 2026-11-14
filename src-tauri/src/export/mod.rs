//! 导出 txt（阶段 10）。
//!
//! 每行 `host:port#<归属地> \| <来源标识>`，归属地为「中文 / English」两段；
//! 归属地缺失时该段留空但保留 `#` 与 ` \| `。
//! 内容为 UTF-8 无 BOM、LF 换行。

use std::path::Path;

use serde::Deserialize;

/// 一条导出行的归属地两段。空串表示该段没有值。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GeoText {
    #[serde(default)]
    pub zh: String,
    #[serde(default)]
    pub en: String,
}

/// 一条待导出的优选地址。`alias` 只用于识别，不进入导出内容。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportRow {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub geo: Option<GeoText>,
}

impl ExportRow {
    /// 表格与勾选共用的键。
    pub fn id(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// 归属地段：`中文 / English`；整段缺失时为空串。
pub fn geo_text(geo: Option<&GeoText>) -> String {
    let Some(geo) = geo else {
        return String::new();
    };
    let zh = geo.zh.trim();
    let en = geo.en.trim();
    if zh.is_empty() && en.is_empty() {
        return String::new();
    }
    format!("{zh} / {en}")
}

/// 按导出格式拼装整份内容；每行以 LF 结尾，末尾也带一个换行。
pub fn render(rows: &[&ExportRow]) -> String {
    let mut text = String::new();
    for row in rows {
        text.push_str(&format!(
            "{}:{}#{} | {}\n",
            row.host.trim(),
            row.port,
            geo_text(row.geo.as_ref()),
            row.source.trim()
        ));
    }
    text
}

/// 范围内没有归属地的行数。
pub fn missing_geo(rows: &[&ExportRow]) -> usize {
    rows.iter()
        .filter(|row| geo_text(row.geo.as_ref()).is_empty())
        .count()
}

/// 选导出范围：有勾选行就只用勾选行，否则用可见行；顺序跟随 `rows`（表格当前排序）。
pub fn select_rows<'a>(
    rows: &'a [ExportRow],
    selected_ids: &[String],
    visible_ids: &[String],
) -> Vec<&'a ExportRow> {
    let ids = if selected_ids.is_empty() {
        visible_ids
    } else {
        selected_ids
    };
    let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    rows.iter()
        .filter(|row| wanted.contains(row.id().as_str()))
        .collect()
}

/// 保存对话框的默认文件名：`bestcf-优选-YYYYMMDD-HHmm.txt`。
pub fn file_name(now: chrono::DateTime<chrono::Local>) -> String {
    now.format("bestcf-优选-%Y%m%d-%H%M.txt").to_string()
}

/// 写文件：UTF-8 无 BOM，换行由 `render` 保证是 LF。
pub fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn geo(zh: &str, en: &str) -> Option<GeoText> {
        Some(GeoText {
            zh: zh.to_string(),
            en: en.to_string(),
        })
    }

    fn row(
        host: &str,
        port: u16,
        alias: &str,
        source: &str,
        geo: Option<GeoText>,
    ) -> ExportRow {
        ExportRow {
            host: host.to_string(),
            port,
            alias: alias.to_string(),
            source: source.to_string(),
            geo,
        }
    }

    fn line_of(rows: &[ExportRow]) -> String {
        let refs: Vec<&ExportRow> = rows.iter().collect();
        render(&refs)
    }

    #[test]
    fn 两段归属地与来源标识按格式拼装() {
        let rows = vec![row(
            "104.16.1.1",
            8443,
            "备注",
            "BestCF·vps789/top10.txt",
            geo("加拿大", "Canada"),
        )];
        assert_eq!(
            line_of(&rows),
            "104.16.1.1:8443#加拿大 / Canada | BestCF·vps789/top10.txt\n"
        );
    }

    #[test]
    fn 归属地两段用斜杠连接() {
        let rows = vec![row(
            "203.0.113.9",
            2053,
            "",
            "BestCF·all.txt",
            geo("日本", "Japan"),
        )];
        assert_eq!(
            line_of(&rows),
            "203.0.113.9:2053#日本 / Japan | BestCF·all.txt\n"
        );
    }

    #[test]
    fn 归属地缺失时该段留空但保留井号与分隔符() {
        let rows = vec![row("104.16.1.2", 8443, "", "BestCF·all.txt", None)];
        assert_eq!(line_of(&rows), "104.16.1.2:8443# | BestCF·all.txt\n");
        let refs: Vec<&ExportRow> = rows.iter().collect();
        assert_eq!(missing_geo(&refs), 1);
    }

    #[test]
    fn 别名里的竖线不影响导出() {
        let rows = vec![row(
            "104.16.1.3",
            8443,
            "东京|高速|备用",
            "BestCF·all.txt",
            geo("加拿大", "Canada"),
        )];
        let text = line_of(&rows);
        assert_eq!(
            text,
            "104.16.1.3:8443#加拿大 / Canada | BestCF·all.txt\n"
        );
        assert!(!text.contains("东京"));
        assert_eq!(text.matches(" | ").count(), 1);
    }

    #[test]
    fn 有勾选时只导出勾选行且顺序跟表格() {
        let rows = vec![
            row("a.example", 443, "", "s1", geo("加拿大", "Canada")),
            row("b.example", 443, "", "s1", geo("日本", "Japan")),
            row("c.example", 443, "", "s2", geo("美国", "United States")),
        ];
        // 表格顺序是 b → a；勾选只勾了 a 与 c。
        let selected = vec!["a.example:443".to_string(), "c.example:443".to_string()];
        let visible = vec![
            "b.example:443".to_string(),
            "a.example:443".to_string(),
            "c.example:443".to_string(),
        ];
        let chosen = select_rows(&rows, &selected, &visible);
        let ids: Vec<String> = chosen.iter().map(|row| row.id()).collect();
        assert_eq!(ids, vec!["a.example:443", "c.example:443"]);
    }

    #[test]
    fn 没有勾选时导出可见行() {
        let rows = vec![
            row("a.example", 443, "", "s1", None),
            row("b.example", 443, "", "s1", None),
            row("c.example", 443, "", "s2", None),
        ];
        let visible = vec!["c.example:443".to_string(), "a.example:443".to_string()];
        let chosen = select_rows(&rows, &[], &visible);
        let ids: Vec<String> = chosen.iter().map(|row| row.id()).collect();
        assert_eq!(ids, vec!["a.example:443", "c.example:443"]);
        // 勾选里的未知地址被忽略，不凭空造行。
        let chosen = select_rows(&rows, &["不存在:1".to_string()], &[]);
        assert!(chosen.is_empty());
    }

    #[test]
    fn 空选择时不导出任何行() {
        let rows = vec![row("a.example", 443, "", "s1", None)];
        assert!(select_rows(&rows, &[], &[]).is_empty());
    }

    #[test]
    fn 缺失归属地的条数按选中范围统计() {
        let rows = vec![
            row("a.example", 443, "", "s1", geo("加拿大", "Canada")),
            row("b.example", 443, "", "s1", None),
            row("c.example", 443, "", "s2", geo("", "")),
        ];
        let chosen = select_rows(
            &rows,
            &[],
            &[
                "a.example:443".to_string(),
                "b.example:443".to_string(),
                "c.example:443".to_string(),
            ],
        );
        // b 与 c 的中英文名都为空，算缺归属地。
        assert_eq!(missing_geo(&chosen), 2);
        assert_eq!(geo_text(Some(&GeoText::default())), "");
    }

    #[test]
    fn 文件名带本地日期与时分() {
        let now = Local.with_ymd_and_hms(2026, 11, 13, 9, 5, 0).unwrap();
        assert_eq!(file_name(now), "bestcf-优选-20261113-0905.txt");
        let now = Local.with_ymd_and_hms(2026, 1, 2, 23, 59, 0).unwrap();
        assert_eq!(file_name(now), "bestcf-优选-20260102-2359.txt");
    }

    #[test]
    fn 导出内容是_utf8_无_bom_且只用_lf_换行() {
        let rows = vec![
            row("a.example", 443, "", "s1", geo("加拿大", "Canada")),
            row("b.example", 443, "", "s1", None),
        ];
        let refs: Vec<&ExportRow> = rows.iter().collect();
        let text = render(&refs);
        assert!(!text.starts_with('\u{feff}'));
        assert!(!text.contains('\r'));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_file(&path, &text).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "不能写 BOM");
        assert!(!bytes.contains(&b'\r'));
        assert_eq!(String::from_utf8(bytes).unwrap(), text);
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 2);
    }
}
