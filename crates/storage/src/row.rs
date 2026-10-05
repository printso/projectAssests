//! 行映射辅助：把 rusqlite 的行读取集中到一处。
//!
//! 目的：JSON 列、可空列、枚举解析这些"易错且重复"的转换只写一遍。
//! 若散落在各 Repository，很容易出现"某处忘了 `.unwrap_or_default()` 导致整页 500"。

use rusqlite::Row;
use serde::de::DeserializeOwned;

use spolia_domain::StorageError;

/// 读取 TEXT 列（非空）。
pub fn text(row: &Row<'_>, idx: usize) -> rusqlite::Result<String> {
    row.get(idx)
}

/// 读取可空 TEXT 列。
pub fn text_opt(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<String>> {
    row.get(idx)
}

/// 读取 INTEGER 列为 usize（负数被钳到 0，防御脏数据）。
pub fn usize_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<usize> {
    let v: i64 = row.get(idx)?;
    Ok(v.max(0) as usize)
}

/// 读取 INTEGER 列为 u32。
pub fn u32_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<u32> {
    let v: i64 = row.get(idx)?;
    Ok(v.clamp(0, i64::from(u32::MAX)) as u32)
}

/// 读取 INTEGER 列为 u8（健康度/星级等）。
pub fn u8_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<u8> {
    let v: i64 = row.get(idx)?;
    Ok(v.clamp(0, 255) as u8)
}

/// 读取 REAL 列。
pub fn real(row: &Row<'_>, idx: usize) -> rusqlite::Result<f64> {
    row.get(idx)
}

/// 读取可空 REAL 列。
pub fn real_opt(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<f64>> {
    row.get(idx)
}

/// 读取 INTEGER 列为 bool（SQLite 无布尔类型）。
pub fn bool_col(row: &Row<'_>, idx: usize) -> rusqlite::Result<bool> {
    let v: i64 = row.get(idx)?;
    Ok(v != 0)
}

/// 反序列化 JSON 列。
///
/// 🔴 关键设计：解析失败时返回**默认值**而非报错。
/// 理由：派生数据（tags / evidence / languages）的格式会随版本演进，
/// 一条旧记录的 JSON 形状变化不应让整个列表页 500。
/// 失败会打 warn 日志（可观测），但页面仍能展示其余字段。
pub fn json_col<T: DeserializeOwned + Default>(row: &Row<'_>, idx: usize) -> rusqlite::Result<T> {
    let raw: Option<String> = row.get(idx)?;
    Ok(parse_json_lenient(raw.as_deref(), idx))
}

/// 宽松解析：失败时返回默认值并记录警告。
pub fn parse_json_lenient<T: DeserializeOwned + Default>(raw: Option<&str>, idx: usize) -> T {
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        return T::default();
    };
    match serde_json::from_str::<T>(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(column = idx, error = %e, "JSON 列解析失败，已降级为默认值");
            T::default()
        }
    }
}

/// 序列化 JSON 列；失败时返回错误。
///
/// 序列化失败几乎只可能来自自定义 Serialize 实现的 bug 或 NaN/Infinity 浮点值。
/// 此时让写入失败（返回错误）比写入坏数据更好，但不应 panic 掉整个进程。
pub fn to_json<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(StorageError::Serde)
}

/// 宽松序列化：失败时返回 `"null"`，用于绝不该阻断写入的派生字段。
pub fn to_json_or_null<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "JSON 序列化失败，写入 null");
        "null".to_string()
    })
}

/// 把 `Option<T>` 映射为可空 JSON 字符串（`None` → SQL NULL）。
pub fn to_json_opt<T: serde::Serialize>(value: &Option<T>) -> Option<String> {
    value.as_ref().map(|v| to_json_or_null(v))
}

/// 当前 UTC 时间戳（RFC3339）。
///
/// 统一在此处生成：避免各处 `chrono::Utc::now()` 格式不一致
/// （有的带毫秒有的不带），导致排序错乱。
pub fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 当前本地日期（YYYY-MM-DD），用于展示"今天"。
pub fn today_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 解析时间戳为 `DateTime<Utc>`；失败返回 `None`（不 panic）。
pub fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            // 兼容 "YYYY-MM-DD" 纯日期（Git 输出格式）
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc())
        })
}

/// 人类可读的相对时间（"2 小时前"）。集中实现，避免各页面格式不一。
pub fn relative_time(ts: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Some(dt) = parse_ts(ts) else {
        return String::new();
    };
    let secs = (now - dt).num_seconds();
    if secs < 0 {
        return "刚刚".to_string();
    }
    if secs < 60 {
        return "刚刚".to_string();
    }
    if secs < 3600 {
        return format!("{} 分钟前", secs / 60);
    }
    if secs < 86400 {
        return format!("{} 小时前", secs / 3600);
    }
    if secs < 86400 * 30 {
        return format!("{} 天前", secs / 86400);
    }
    if secs < 86400 * 365 {
        return format!("{} 个月前", secs / (86400 * 30));
    }
    format!("{} 年前", secs / (86400 * 365))
}

/// 字节数格式化为人类可读（设置页"当前占用"）。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn lenient_json_returns_default_on_garbage() {
        let v: Vec<String> = parse_json_lenient(Some("not json{{"), 0);
        assert!(v.is_empty());
    }

    #[test]
    fn lenient_json_handles_null_and_empty() {
        let v: Vec<String> = parse_json_lenient(None, 0);
        assert!(v.is_empty());
        let v2: Vec<String> = parse_json_lenient(Some("   "), 0);
        assert!(v2.is_empty());
    }

    #[test]
    fn lenient_json_parses_valid() {
        let v: Vec<String> = parse_json_lenient(Some(r#"["a","b"]"#), 0);
        assert_eq!(v, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn to_json_or_null_never_panics() {
        assert_eq!(to_json_or_null(&vec!["a"]), r#"["a"]"#);
        // f64::NAN 无法序列化为 JSON，应降级为 "null" 而非 panic
        assert_eq!(to_json_or_null(&f64::NAN), "null");
    }

    #[test]
    fn to_json_opt_maps_none_to_none() {
        let none: Option<Vec<String>> = None;
        assert!(to_json_opt(&none).is_none());
        let some = Some(vec!["x".to_string()]);
        assert_eq!(to_json_opt(&some).as_deref(), Some(r#"["x"]"#));
    }

    #[test]
    fn timestamps_are_rfc3339_zulu() {
        let t = now_utc();
        assert!(t.ends_with('Z'), "{t}");
        assert!(parse_ts(&t).is_some());
    }

    #[test]
    fn parse_ts_accepts_date_only() {
        assert!(parse_ts("2025-05-20").is_some());
        assert!(parse_ts("garbage").is_none());
    }

    #[test]
    fn relative_time_buckets() {
        let now = chrono::Utc::now();
        let mk = |d: i64| (now - chrono::Duration::seconds(d)).to_rfc3339();
        assert_eq!(relative_time(&mk(10), now), "刚刚");
        assert_eq!(relative_time(&mk(300), now), "5 分钟前");
        assert_eq!(relative_time(&mk(7200), now), "2 小时前");
        assert_eq!(relative_time(&mk(86400 * 3), now), "3 天前");
        assert_eq!(relative_time("bad", now), "");
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(214 * 1024 * 1024), "214.0 MB");
    }

    /// 行映射辅助函数必须在真实行上工作。
    #[test]
    fn row_helpers_read_typed_columns() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t(a TEXT, b INTEGER, c REAL, d INTEGER, e TEXT);
             INSERT INTO t VALUES('x', -5, 1.5, 1, '[\"k\"]');",
        )
        .unwrap();
        c.query_row("SELECT a,b,c,d,e FROM t", [], |row| {
            assert_eq!(text(row, 0)?, "x");
            assert_eq!(usize_col(row, 1)?, 0, "负数应钳为 0");
            assert!((real(row, 2)? - 1.5).abs() < f64::EPSILON);
            assert!(bool_col(row, 3)?);
            let tags: Vec<String> = json_col(row, 4)?;
            assert_eq!(tags, vec!["k".to_string()]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn u8_and_u32_columns_clamp() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE t(v INTEGER); INSERT INTO t VALUES(999);",
        )
        .unwrap();
        c.query_row("SELECT v FROM t", [], |row| {
            assert_eq!(u8_col(row, 0)?, 255, "u8 应钳到上界");
            assert_eq!(u32_col(row, 0)?, 999);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn today_local_is_iso_date() {
        let d = today_local();
        assert_eq!(d.len(), 10, "{d}");
        assert_eq!(d.chars().nth(4), Some('-'));
    }
}
