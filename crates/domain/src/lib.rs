//! Spolia 领域模型。
//!
//! 本 crate 是**纯类型定义层**：只有数据结构与不变式，不含任何 IO（数据库 / 网络 / 文件系统）。
//! 字段命名严格对齐《技术设计书》§11 的 SQLite schema（snake_case 通过 serde 显式声明），
//! 使 API 返回体与数据库列名、前端字段三者完全一致，避免"三套命名"的翻译成本。
//!
//! # 分层约定
//! ```text
//! domain   ← 类型（本 crate，被所有层依赖）
//! storage  ← 持久化（依赖 domain）
//! scanner/asset/insight/search ← 引擎（依赖 domain + storage）
//! ai/mcp/jobs ← 编排与出口
//! server   ← HTTP/IPC 边界
//! ```

mod analyst;
mod asset;
mod capability;
mod error;
mod feedback;
mod graph;
mod insight;
mod job;
mod opportunity;
mod project;
mod relation;
mod search;
mod settings;
mod time;

pub use analyst::*;
pub use asset::*;
pub use capability::*;
pub use error::*;
pub use feedback::*;
pub use graph::*;
pub use insight::*;
pub use job::*;
pub use opportunity::*;
pub use project::*;
pub use relation::*;
pub use search::*;
pub use settings::*;
pub use time::*;

/// 当前 schema 版本号。数据库迁移以此为基准，任何破坏性变更都必须 +1 并提供迁移脚本。
///
/// 版本历史：
/// - v1：初始 schema（11 张表 + projects/assets/capabilities 三张 FTS5 索引）
/// - v2：新增 `insights_fts` / `opportunities_fts`，让洞察与机会可被检索（并回填存量数据，否则老库升级后旧洞察仍然搜不到）
/// - v3：`audit_log` 新增 `ok` / `error` 两列，让审计能区分"模型调用成功/失败"，
///   并把失败的云端调用也纳入留痕（prompt 已出网却被网关拒绝的情况此前完全无痕）
pub const SCHEMA_VERSION: i32 = 3;

/// 产品品牌元信息（单机版，无账号体系）。
pub const BRAND: Brand = Brand {
    name: "Spolia",
    sub: "Your Personal R&D OS",
    logo: "S",
    tagline: "让过去的每一个项目，都成为你未来的可能性",
};

/// 品牌常量，供 API `/brand` 与前端标题使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brand {
    pub name: &'static str,
    pub sub: &'static str,
    pub logo: &'static str,
    pub tagline: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 保证 schema 常量与品牌信息不会被误改。
    ///
    /// 这里断言**确切**版本号而非范围：改动 schema 却忘记写迁移脚本时，
    /// 这个测试会失败，强制开发者同时更新 `spolia-storage` 的迁移表。
    ///
    /// 🔴 新增迁移时必须把这里的数字一起改掉，并在下面的注释里补一行版本说明。
    /// 这是刻意的"双处修改"摩擦：它逼着提交者意识到自己在改数据库结构。
    #[test]
    fn brand_and_schema_are_stable() {
        assert_eq!(BRAND.name, "Spolia");
        assert_eq!(BRAND.logo, "S");
        assert_eq!(SCHEMA_VERSION, 3);
    }
}
