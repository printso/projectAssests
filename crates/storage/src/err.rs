//! `rusqlite::Error` → `StorageError` 的**唯一分类点**。
//!
//! # 🔴 为什么要有这个文件
//! 各 Repository 里散落着 280+ 处 `StorageError::sqlite(ctx, e)`，
//! 它把驱动错误**字符串化**塞进 `reason`。字符串化之后，
//! "数据库正忙（可重试）"与"SQL 写错了（程序缺陷）"就再也分不开了——
//! 两者都变成 500，用户看到的都是"数据库操作失败"。
//!
//! 实测后果（合成大项目、22000 资产规模）：后台索引期间用户改设置，
//! 39 次写入里有 3 次撞上写锁，等满 `busy_timeout`(5s) 后返回
//! **500 storage_error** 并写进 ERROR 日志。
//! 而这是完全正常的并发瞬时状态，本该是 **409 + "稍后重试"**。
//!
//! # 分类必须在类型层面做
//! 唯一可靠的判据是 SQLite 的**错误码**
//! （`ErrorCode::DatabaseBusy` / `DatabaseLocked`）。
//!
//! ⚠️ 不要用 `reason.contains("locked")` 之类的子串匹配：
//! 文案随 SQLite 版本与编译期语言设置变化，一改就静默失效，
//! 且没有任何编译期保护。本项目已有明令禁止此写法的先例
//! （见 `service/context.rs` 中 `Job(AlreadyRunning)` 的注释）。

use rusqlite::{Error as SqliteError, ErrorCode};

use projectassests_domain::StorageError;

/// 把驱动错误转成领域错误，**并识别锁竞争**。
///
/// 写路径（尤其用户可触发的设置写入）应使用本函数而非
/// [`StorageError::sqlite`]，后者收 `impl Display`、拿不到错误码，无法分类。
///
/// # 非 busy 的错误为什么直接用 `err.to_string()`
/// 曾经这里有一份 40 行的自定义中文格式化，按变体逐个翻译
/// （`InvalidColumnType` → "第 {i} 列 {name} 类型不符"…）。已删除，因为：
///
/// 1. **没有信息增量**。rusqlite 的 `Display for Error` 本身就带上了
///    列名、列序号、类型、具体值（见其源码 `impl fmt::Display`），
///    例如 `Invalid column type Null at index: 2, name: created_at`。
///    翻译只是换个说法，排查时需要的字段一个都没多。
/// 2. **它会随 rusqlite 版本漂移**。变体改名/新增时这里要么编译失败，
///    要么落到兜底分支静默降级——已经踩过一次（`FromSql` 实际叫
///    `FromSqlConversionFailure`，猜错就编译不过）。
/// 3. 违反简洁原则：多一份映射就多一处要维护、要测试、会与上游不一致的地方。
pub fn sqlite_err(context: impl Into<String>, err: SqliteError) -> StorageError {
    let context = context.into();

    // 🔴 用 `sqlite_error_code()` 而非手动 match `SqliteFailure`：
    // 它是 rusqlite 提供的官方取码入口，将来若错误包装形式变化，
    // 这里不用跟着改。
    if matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy) | Some(ErrorCode::DatabaseLocked)
    ) {
        // 锁竞争：可重试的瞬时状态，不是故障。
        // reason 刻意不放进 Busy（"database is locked" 是唯一可能原因，
        // 重复它没有信息量）；上下文进日志已足够定位是哪一步撞锁。
        tracing::debug!(context = %context, "数据库写锁竞争");
        return StorageError::Busy { context };
    }

    StorageError::Sqlite {
        context,
        reason: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ffi;

    /// 构造一个带指定主错误码的 `SqliteFailure`。
    ///
    /// 🔴 **不能**写 `ffi::Error::new(code as i32)`：`ErrorCode` 没有 `#[repr]`，
    /// `code as i32` 得到的是 Rust 按声明顺序分配的判别值（0,1,2…），
    /// 而 `Error::new` 要的是 **SQLite 原始结果码**（`SQLITE_BUSY = 5`）。
    /// 两者错位会让测试反向失败：`DatabaseBusy` 认不出、`ReadOnly` 反被当成 Busy。
    /// `ffi::Error` 的字段是 `pub`，直接构造最可靠，也不依赖常量是否被重导出。
    fn failure(code: ErrorCode, msg: &str) -> SqliteError {
        SqliteError::SqliteFailure(
            ffi::Error {
                code,
                extended_code: 0,
            },
            Some(msg.into()),
        )
    }

    /// 🔴 锁竞争必须被识别为 `Busy`，而不是混进通用 `Sqlite`。
    ///
    /// 这是本文件存在的唯一理由：分不出来，上层就无法把它映射成 409。
    #[test]
    fn busy_error_code_is_classified_as_busy() {
        for code in [ErrorCode::DatabaseBusy, ErrorCode::DatabaseLocked] {
            let out = sqlite_err("保存扫描设置", failure(code, "database is locked"));
            assert!(out.is_busy(), "{code:?} 应被分类为 Busy，实际 {out:?}");
            assert!(
                matches!(&out, StorageError::Busy { context } if context == "保存扫描设置"),
                "context 必须原样保留供诊断: {out:?}"
            );
        }
    }

    /// 🔴 **反向断言**：其他错误码绝不能被误判成 Busy。
    ///
    /// 只测"busy 被识别"的话，一个把分类写成恒真的变异照样通过——
    /// 那会让**所有**数据库故障都变成 409"稍后重试"，
    /// 用户被引导去重试一个永远不会成功的操作（SQL 写错、约束冲突、库损坏）。
    ///
    /// ⚠️ 用例只能用 `ErrorCode` 的**真实变体**。
    /// 这里曾写过 `NoSuchTable`——它其实是 SQLite 的*扩展*错误码
    /// （`SQLITE_ERROR_NO_SUCH_TABLE`），不在 rusqlite 的主 `ErrorCode` 枚举里，
    /// 编译期直接报错。凭印象写错误码名是行不通的。
    #[test]
    fn non_busy_error_codes_are_not_busy() {
        let cases = [
            (ErrorCode::ConstraintViolation, "UNIQUE constraint failed: projects.path"),
            (ErrorCode::ReadOnly, "attempt to write a readonly database"),
            (ErrorCode::DatabaseCorrupt, "database disk image is malformed"),
            (ErrorCode::PermissionDenied, "unable to open database file"),
            (ErrorCode::OutOfMemory, "out of memory"),
            (ErrorCode::DiskFull, "database or disk is full"),
            (ErrorCode::OperationInterrupted, "interrupted"),
            (ErrorCode::InternalMalfunction, "internal logic error"),
        ];
        for (code, msg) in cases {
            let out = sqlite_err("写入项目", failure(code, msg));
            assert!(!out.is_busy(), "{code:?} 不该被判为 Busy: {out:?}");
            // reason 必须保留：这是排查"约束为什么冲突"的唯一线索
            match &out {
                StorageError::Sqlite { reason, .. } => {
                    assert_eq!(reason, msg, "reason 应原样保留: {out:?}")
                }
                other => panic!("应为 Sqlite 变体: {other:?}"),
            }
        }
    }

    /// 约束冲突是最常见的"非 busy 写失败"，单独锁一条：
    /// 它必须是 500 类（数据/程序问题），绝不能变成"稍后重试"。
    #[test]
    fn constraint_violation_keeps_reason() {
        let out = sqlite_err(
            "写入关系",
            failure(ErrorCode::ConstraintViolation, "FOREIGN KEY constraint failed"),
        );
        assert!(!out.is_busy());
        assert!(
            out.to_string().contains("FOREIGN KEY"),
            "诊断信息必须可见: {out}"
        );
    }

    /// 非 `SqliteFailure` 的错误也要保留 rusqlite 自带的诊断字段
    /// （列名/列序号/类型），证明"不再自定义格式化"没有丢信息。
    #[test]
    fn non_failure_errors_keep_rusqlite_diagnostics() {
        let out = sqlite_err(
            "读取列",
            SqliteError::InvalidColumnType(
                2,
                "created_at".into(),
                rusqlite::types::Type::Null,
            ),
        );
        let s = out.to_string();
        assert!(s.contains("created_at"), "应带上列名: {s}");
        assert!(s.contains('2'), "应带上列序号: {s}");
        assert!(s.contains("Null"), "应带上实际类型: {s}");

        let out = sqlite_err("执行查询", SqliteError::ExecuteReturnedResults);
        assert!(!out.to_string().is_empty());
    }

    /// `sqlite_err` 与 `StorageError::sqlite` 的分工必须清楚：
    /// 后者拿不到错误码，因此**永远不会**产出 Busy。
    /// 这条测试钉住"写路径必须用 sqlite_err"这个约定。
    #[test]
    fn plain_sqlite_helper_never_yields_busy() {
        let out = StorageError::sqlite("保存扫描设置", "database is locked");
        assert!(
            !out.is_busy(),
            "StorageError::sqlite 无法分类 busy；写路径必须改用 sqlite_err"
        );
    }
}
