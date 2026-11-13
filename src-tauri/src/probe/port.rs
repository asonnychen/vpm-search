//! 本地入站端口选取：20000–29999 内找第一个没被占用的端口。

use std::net::TcpListener;

/// 本地 mixed 入站的端口区间，闭区间。
pub const PORT_START: u16 = 20_000;
pub const PORT_END: u16 = 29_999;

/// 在给定区间内挑第一个没被占用的端口。
///
/// `is_taken` 让测试可以注入占用情况，生产环境用真实 bind 探测。
pub fn pick_free_port_in(
    start: u16,
    end: u16,
    is_taken: impl Fn(u16) -> bool,
) -> Option<u16> {
    (start..=end).find(|port| !is_taken(*port))
}

/// 端口当前是否已被占用。绑不上就认为被占。
fn is_taken(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// 在 20000–29999 内挑一个空闲端口，被占则顺延到下一个。
pub fn pick_free_port() -> Option<u16> {
    pick_free_port_in(PORT_START, PORT_END, is_taken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 从区间起点开始挑第一个空闲端口() {
        assert_eq!(pick_free_port_in(20_000, 20_010, |_| false), Some(20_000));
    }

    #[test]
    fn 端口被占时自动顺延() {
        let taken = |port: u16| (20_000..=20_005).contains(&port);
        assert_eq!(pick_free_port_in(20_000, 20_010, taken), Some(20_006));
    }

    #[test]
    fn 整段都被占时返回_none() {
        assert_eq!(pick_free_port_in(20_000, 20_002, |_| true), None);
    }

    #[test]
    fn 生产区间落在_20000_到_29999() {
        assert_eq!(PORT_START, 20_000);
        assert_eq!(PORT_END, 29_999);
    }
}
