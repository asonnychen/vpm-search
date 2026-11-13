//! 日志脱敏。
//!
//! 节点凭据（UUID、password、token、secret）不允许出现在任何日志里，
//! 一律替换成 `****`。规则对整行文本生效，操作日志、应用日志 JSON 行
//! 与 sing-box 输出都走同一个函数。

/// 替换后的占位符。
pub const MASK: &str = "****";

/// 需要替换值的键名，全部小写比较。
const SECRET_KEYS: [&str; 4] = ["password", "token", "uuid", "secret"];

/// 把一行文本里的凭据替换成 `****`。
pub fn redact(input: &str) -> String {
    let masked = mask_uuids(input);
    mask_key_values(&masked)
}

/// 是否是 UUID 形状的 36 个字符（8-4-4-4-12 位十六进制，连字符分隔）。
fn is_uuid(bytes: &[u8], start: usize) -> bool {
    const DASHES: [usize; 4] = [8, 13, 18, 23];
    if start + 36 > bytes.len() {
        return false;
    }
    for (offset, byte) in bytes[start..start + 36].iter().enumerate() {
        if DASHES.contains(&offset) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn mask_uuids(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if is_uuid(bytes, i) {
            out.push_str(MASK);
            i += 36;
            continue;
        }
        // 逐字符复制，避免切断多字节字符。
        let ch = input[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 找到 `key` 之后的值区间，返回 `(值开始, 值结束)`。
fn value_range(text: &str, key_end: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = key_end;
    // 允许 `"key"` 这种带引号的形式。
    if bytes.get(i) == Some(&b'"') {
        i += 1;
    }
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    if !matches!(bytes.get(i), Some(b'=') | Some(b':')) {
        return None;
    }
    i += 1;
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let quoted = matches!(bytes.get(i), Some(b'"') | Some(b'\''));
    if quoted {
        i += 1;
    }
    let start = i;
    while let Some(byte) = bytes.get(i) {
        if quoted && *byte == b'\\' {
            // 跳过转义字符，避免在 \" 处提前截断 JSON 字符串。
            i += 2;
            continue;
        }
        let stop = if quoted {
            matches!(byte, b'"' | b'\'')
        } else {
            matches!(byte, b',' | b'&' | b';' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n')
        };
        if stop {
            break;
        }
        i += 1;
    }
    if i == start {
        return None;
    }
    Some((start, i))
}

fn mask_key_values(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let start = from + found;
            let end = start + key.len();
            // 键名必须是独立单词：前面不是字母数字，后面紧跟引号、空白或分隔符。
            let before_ok = start == 0
                || !bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_';
            let after_ok = bytes
                .get(end)
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_');
            if before_ok && after_ok {
                if let Some(range) = value_range(input, end) {
                    ranges.push(range);
                }
            }
            from = end;
        }
    }

    if ranges.is_empty() {
        return input.to_string();
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        out.push_str(&input[cursor..start]);
        out.push_str(MASK);
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 键值形式的_uuid_被替换() {
        assert_eq!(
            redact("节点 uuid=550e8400-e29b-41d4-a716-446655440000 已保存"),
            "节点 uuid=**** 已保存"
        );
    }

    #[test]
    fn 裸_uuid_也被替换且大小写不敏感() {
        assert_eq!(
            redact("见 550E8400-E29B-41D4-A716-446655440000 记录"),
            "见 **** 记录"
        );
    }

    #[test]
    fn 密码与_token_的值被替换() {
        assert_eq!(
            redact(r#"{"password":"hunter2","token":"abc123"}"#),
            r#"{"password":"****","token":"****"}"#
        );
    }

    #[test]
    fn 键名大小写不敏感() {
        assert_eq!(redact("Password: s3cret"), "Password: ****");
    }

    #[test]
    fn 普通文本原样保留() {
        assert_eq!(redact("抓取 9 个源，解析 233 条"), "抓取 9 个源，解析 233 条");
        assert_eq!(redact("延迟 38ms"), "延迟 38ms");
    }

    #[test]
    fn 多个凭据同时出现() {
        assert_eq!(
            redact("password=1&token=2&host=example.com"),
            "password=****&token=****&host=example.com"
        );
    }

    #[test]
    fn 值里有转义引号也能整段替换() {
        let line = r#"{"password":"ab\"cd","host":"example.com"}"#;
        assert_eq!(redact(line), r#"{"password":"****","host":"example.com"}"#);
    }
}
