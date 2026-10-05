//! 时间解析与"距今天数"——全项目唯一实现。
//!
//! # 为什么放在 domain 层
//! 项目状态推断（`Project::infer_status`）、健康度（`compute_health`）、
//! 洞察引擎的"遗忘资产"检测、搜索的新鲜度排序，都需要把
//! `YYYY-MM-DD` / RFC3339 字符串换算成"距今天数"。
//! 若各 crate 各写一份，口径必然漂移——例如一处把无法解析当 0 天（显示成"今天刚更新"），
//! 另一处当 `None`（显示成"未知"），同一份数据在不同页面会自相矛盾。
//!
//! # 两条纪律
//! 1. **`now` 一律注入**，不在库内部读时钟。否则同一份数据两次调用结果不同，
//!    单元测试无法固定期望值，快照比对也会随机失败。
//! 2. **解析失败返回 `None`，绝不返回 0**。0 意味着"今天"，
//!    把脏数据显示成"刚刚更新"比显示"未知"危险得多。

/// 解析日期字符串。支持两种格式：
/// - RFC3339（`2026-09-29T12:00:00Z`、带时区偏移）——Git 提交时间
/// - 纯日期（`2026-09-29`）——按当天 00:00 UTC 计
///
/// 无法解析返回 `None`。
pub fn parse_date(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
}

/// 解析可空日期字段（数据库里的 `Option<String>` 列）。
pub fn parse_date_opt(s: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    parse_date(s?)
}

/// 距 `now` 的天数。无法解析返回 `None`（**不是** 0）。
///
/// 未来时间钳为 0：用户机器时钟偏移或 Git 提交时间在未来时，
/// 显示"距今 -3 天"毫无意义，"今天"是可接受的降级。
pub fn days_since(
    date: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<i64> {
    parse_date_opt(date).map(|dt| (now - dt).num_days().max(0))
}

/// 从多个候选日期中取**较近**的一个的天数。
///
/// 典型用途：项目活跃度既可能来自 Git 提交时间，也可能来自文件 mtime，
/// 取较近者才不会把"有 mtime 但无 Git"的项目误判为陈旧。
/// 全部无法解析时返回 `None`。
pub fn days_since_latest(
    candidates: &[Option<&str>],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<i64> {
    candidates
        .iter()
        .filter_map(|c| days_since(*c, now))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn parses_plain_date() {
        let d = parse_date("2026-09-26");
        assert!(d.is_some());
        assert_eq!(days_since(Some("2026-09-26"), now()), Some(3));
    }

    #[test]
    fn parses_rfc3339_with_offset() {
        assert!(parse_date("2026-09-29T12:00:00+08:00").is_some());
        assert!(parse_date("2026-09-29T12:00:00Z").is_some());
    }

    /// 🔴 解析失败必须返回 None，不得当成"今天"。
    #[test]
    fn unparseable_returns_none_not_zero() {
        assert_eq!(parse_date("garbage"), None);
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("   "), None);
        assert_eq!(days_since(Some("garbage"), now()), None);
        assert_eq!(days_since(None, now()), None);
        // 常见误写：斜杠分隔
        assert_eq!(parse_date("2026/09/26"), None);
    }

    #[test]
    fn future_date_clamps_to_zero() {
        assert_eq!(days_since(Some("2027-01-01"), now()), Some(0));
    }

    #[test]
    fn same_day_is_zero() {
        assert_eq!(days_since(Some("2026-09-29"), now()), Some(0));
        assert_eq!(days_since(Some("2026-09-29T23:59:00Z"), now()), Some(0));
    }

    #[test]
    fn latest_picks_the_newest_candidate() {
        let cands = [Some("2026-09-20"), Some("2026-09-28"), None];
        assert_eq!(days_since_latest(&cands, now()), Some(1), "应取较近者");
    }

    #[test]
    fn latest_all_unknown_is_none() {
        assert_eq!(days_since_latest(&[None, Some("bad")], now()), None);
        assert_eq!(days_since_latest(&[], now()), None);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert!(parse_date("  2026-09-26  ").is_some());
    }

    #[test]
    fn parse_date_opt_handles_none() {
        assert_eq!(parse_date_opt(None), None);
        assert!(parse_date_opt(Some("2026-09-26")).is_some());
    }
}
