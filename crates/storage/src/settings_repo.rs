//! 设置 Repository。
//!
//! 单机版无账号体系，所有配置只存本机 SQLite。
//! 🔒 敏感键（API Key）单独标记 `sensitive=1`，导出诊断信息时可据此排除。

use rusqlite::{params, OptionalExtension};

use projectassests_domain::{
    AppearanceSettings, AuditEntry, LlmSettings, RouteTarget, ScanSettings, Settings, StorageError,
};

use crate::err::sqlite_err;
use crate::pool::Pool;
use crate::row::{self, now_utc};

/// 设置键名（集中定义，避免拼写漂移）。
mod keys {
    pub const LLM: &str = "llm";
    pub const SCAN: &str = "scan";
    pub const APPEARANCE: &str = "appearance";
    /// API Key 单独存，标记为敏感
    pub const API_KEY: &str = "llm.api_key";
}

/// 设置仓储。
#[derive(Debug)]
pub struct SettingsRepo<'a> {
    pool: &'a Pool,
}

impl<'a> SettingsRepo<'a> {
    pub fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// 读取全部设置。首次运行（无任何记录）返回 `Ok(None)`，
    /// 由调用方决定是否写入默认值——存储层不擅自初始化，保持"读"的纯粹。
    /// 读取全部设置。
    ///
    /// 返回 `None` 仅当**所有**设置段都不存在（真正的首次运行）。
    ///
    /// 🔴 不能用"LLM 段缺失"作为首次运行的判据。
    /// `save_scan` / `save_appearance` 是分段写入的，用户完全可能
    /// 只添加了扫描目录而从未配置大模型——此时 LLM 行不存在，
    /// 若据此返回 `None`，`get_or_default` 会给出空默认值，
    /// 用户刚加的目录就"凭空消失"了（扫描任务随即报"没有授权目录"）。
    /// 分段读取、分段兜底才是正确语义。
    pub fn get_all(&self) -> Result<Option<Settings>, StorageError> {
        let conn = self.pool.get()?;

        let llm_raw: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [keys::LLM], |r| r.get(0))
            .optional()
            .map_err(|e| StorageError::sqlite("读取 llm 设置", e))?;
        let scan_raw: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [keys::SCAN], |r| r.get(0))
            .optional()
            .map_err(|e| StorageError::sqlite("读取 scan 设置", e))?;
        let appearance_raw: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [keys::APPEARANCE], |r| r.get(0))
            .optional()
            .map_err(|e| StorageError::sqlite("读取 appearance 设置", e))?;

        if llm_raw.is_none() && scan_raw.is_none() && appearance_raw.is_none() {
            return Ok(None); // 真正的首次运行
        }

        let mut llm: LlmSettings = row::parse_json_lenient(llm_raw.as_deref(), 0);
        // API Key 单独读（可能未设置）
        let api_key: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [keys::API_KEY], |r| r.get(0))
            .optional()
            .map_err(|e| StorageError::sqlite("读取 api_key", e))?;
        llm.cloud_api_key = api_key.unwrap_or_default();

        let scan: ScanSettings = row::parse_json_lenient(scan_raw.as_deref(), 0);
        let appearance: AppearanceSettings = row::parse_json_lenient(appearance_raw.as_deref(), 0);

        Ok(Some(Settings { llm, scan, appearance }))
    }

    /// 读取设置，无则返回默认值（便捷方法）。
    pub fn get_or_default(&self) -> Result<Settings, StorageError> {
        Ok(self.get_all()?.unwrap_or_default())
    }

    /// 保存全部设置（整体覆盖）。API Key 拆到独立的敏感键。
    pub fn save(&self, s: &Settings) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        // 🔴 事务开启/提交都是写路径，必须用 `sqlite_err` 才能分类出 `Busy`：
        // 后台索引持锁时，`unchecked_transaction()` 正是会等满 busy_timeout 的那一步。
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| sqlite_err("开启设置事务", e))?;
        let now = now_utc();

        // LLM 设置：先把 api_key 从序列化体里摘出，避免明文 key 混进普通配置
        let mut llm_wo_key = s.llm.clone();
        llm_wo_key.cloud_api_key = String::new();
        Self::put(&tx, keys::LLM, &row::to_json(&llm_wo_key)?, false, &now)?;
        // API Key 单独存并标记敏感
        Self::put(&tx, keys::API_KEY, &s.llm.cloud_api_key, true, &now)?;
        Self::put(&tx, keys::SCAN, &row::to_json(&s.scan)?, false, &now)?;
        Self::put(&tx, keys::APPEARANCE, &row::to_json(&s.appearance)?, false, &now)?;

        tx.commit().map_err(|e| sqlite_err("提交设置事务", e))
    }

    /// 只更新 LLM 设置（设置页「保存配置」通常只动这一块）。
    pub fn save_llm(&self, llm: &LlmSettings) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| sqlite_err("开启 LLM 设置事务", e))?;
        let now = now_utc();
        let mut wo_key = llm.clone();
        wo_key.cloud_api_key = String::new();
        Self::put(&tx, keys::LLM, &row::to_json(&wo_key)?, false, &now)?;
        Self::put(&tx, keys::API_KEY, &llm.cloud_api_key, true, &now)?;
        tx.commit().map_err(|e| sqlite_err("提交 LLM 设置事务", e))
    }

    /// 只更新扫描设置。
    pub fn save_scan(&self, scan: &ScanSettings) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        Self::put(&conn, keys::SCAN, &row::to_json(scan)?, false, &now_utc())
    }

    /// 只更新外观设置。
    pub fn save_appearance(&self, appearance: &AppearanceSettings) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        Self::put(&conn, keys::APPEARANCE, &row::to_json(appearance)?, false, &now_utc())
    }

    /// 写入 API Key（掩码占位符的处理在 API 层，这里只管持久化）。
    pub fn save_api_key(&self, key: &str) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        Self::put(&conn, keys::API_KEY, key, true, &now_utc())
    }

    /// 读取 API Key（明文，仅供后端调用 LLM 时使用；**绝不**返回给前端）。
    pub fn get_api_key(&self) -> Result<String, StorageError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [keys::API_KEY], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .map(|v| v.unwrap_or_default())
        .map_err(|e| StorageError::sqlite("读取 api_key", e))
    }

    fn put(conn: &rusqlite::Connection, key: &str, value: &str, sensitive: bool, now: &str) -> Result<(), StorageError> {
        conn.execute(
            "INSERT INTO settings (key, value, sensitive, updated_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, sensitive=excluded.sensitive, updated_at=excluded.updated_at",
            params![key, value, if sensitive { 1 } else { 0 }, now],
        )
        // 🔴 走 `sqlite_err` 而非 `StorageError::sqlite`：设置写入是**用户可触发**的，
        // 而后台索引可能正持有写锁。分类出 `Busy` 之后上层才能回 409「稍后重试」，
        // 而不是让用户看到 500「数据库操作失败」（那看起来像程序坏了）。
        // 实测：索引 22000 资产的大项目时，39 次设置写入有 3 次撞锁等满 5s。
        .map_err(|e| sqlite_err(format!("写入设置 {key}"), e))?;
        Ok(())
    }

    /// 追加网络访问审计条目（《技术设计书》§23「可审计」）。
    ///
    /// 🔴 `ok` 是三态：`Some(true)` 成功、`Some(false)` 失败、`None` 不是模型调用
    /// （本地安全事件）。存成 INTEGER 列，NULL 即 `None`。
    /// 语义与回填规则见 `schema.rs` 的 V3 注释——改动前务必先读那里。
    pub fn audit(&self, entry: &AuditEntry) -> Result<(), StorageError> {
        let conn = self.pool.get()?;
        conn.execute(
            "INSERT INTO audit_log (at, model, route, job_type, summary, project_id, ok, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                entry.at,
                entry.model,
                entry.route.as_str(),
                entry.job_type,
                entry.summary,
                entry.project_id,
                // bool → INTEGER：rusqlite 支持 Option<bool>，None 落成 NULL
                entry.ok,
                entry.error,
            ],
        )
        // 审计写入同样可能与索引撞锁；且审计**不可丢失**（Local-First 的可信凭证），
        // 分类成 Busy 让调用方能区分"稍后重试"与"真的写失败了"。
        .map_err(|e| sqlite_err("写入审计日志", e))?;
        Ok(())
    }

    /// 是否存在**至少一次成功的模型调用**。
    ///
    /// 🔴 为什么不能用「`recent_audit(1)` 非空」代替：
    /// v3 之前审计只在成功时写入，所以"有任何记录"确实等价于"跑通过一次"。
    /// 现在**失败也留痕**，那条推理就不成立了——一次 400 未开通的调用
    /// 同样会写一条记录，首页引导却会因此把「配置大模型」标成已完成，
    /// 而那恰恰证明模型**不可用**。这正是本方法存在的原因。
    ///
    /// 用 SQL 而非拉取记录到内存过滤：审计日志只增不减，
    /// "最近 N 条里有没有成功的"依赖 N 的取值，是个会随数据增长而悄悄变错的问题。
    /// `WHERE ok = 1 LIMIT 1` 则与日志规模无关。
    ///
    /// 🔴 必须是 `ok = 1` 而不是 `ok IS NOT NULL`：
    /// `ok = 0`（失败）与 `ok IS NULL`（本地安全事件）都不能算"模型可用过"。
    pub fn has_successful_llm_call(&self) -> Result<bool, StorageError> {
        let conn = self.pool.get()?;
        let found: Option<i32> = conn
            .query_row(
                "SELECT 1 FROM audit_log WHERE ok = 1 LIMIT 1",
                [],
                |r| r.get(0),
            )
            // 无匹配行时 query_row 返回 QueryReturnedNoRows —— 那是正常的"没有"，
            // 不是故障，归一成 None 即可。其余错误才向上抛。
            .optional()
            .map_err(|e| StorageError::sqlite("查询成功调用记录", e))?;
        Ok(found.is_some())
    }

    /// 最近 N 条审计记录。
    ///
    /// 🔴 只用于**展示**（设置页「数据与隐私」列表）。
    /// 不要用它的结果推断"模型是否可用过"——失败记录也在里面，
    /// 判定请用 [`Self::has_successful_llm_call`]。
    pub fn recent_audit(&self, limit: u32) -> Result<Vec<AuditEntry>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare(
                "SELECT at, model, route, job_type, summary, project_id, ok, error FROM audit_log
                 ORDER BY at DESC, id DESC LIMIT ?1",
            )
            .map_err(|e| StorageError::sqlite("准备审计查询", e))?;
        let rows = stmt
            .query_map(params![i64::from(limit.clamp(1, 500))], |r| {
                let route_str: String = r.get(2)?;
                Ok(AuditEntry {
                    at: r.get(0)?,
                    model: r.get(1)?,
                    route: RouteTarget::parse(&route_str).unwrap_or_default(),
                    job_type: r.get(3)?,
                    summary: r.get(4)?,
                    project_id: r.get(5)?,
                    ok: r.get(6)?,
                    error: r.get(7)?,
                })
            })
            .map_err(|e| StorageError::sqlite("执行审计查询", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射审计行", e))?);
        }
        Ok(out)
    }

    /// 非敏感设置的键值对（诊断导出用，排除 API Key）。
    pub fn export_non_sensitive(&self) -> Result<Vec<(String, String)>, StorageError> {
        let conn = self.pool.get()?;
        let mut stmt = conn
            .prepare("SELECT key, value FROM settings WHERE sensitive = 0 ORDER BY key")
            .map_err(|e| StorageError::sqlite("准备设置导出", e))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| StorageError::sqlite("执行设置导出", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| StorageError::sqlite("映射设置导出行", e))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use projectassests_domain::{CloudProvider, LocalBackend, ScanDir, Theme};

    fn db() -> Database {
        Database::in_memory().unwrap()
    }

    #[test]
    fn fresh_db_has_no_settings() {
        assert!(db().settings().get_all().unwrap().is_none());
    }

    #[test]
    fn get_or_default_returns_defaults() {
        let s = db().settings().get_or_default().unwrap();
        assert!(s.llm.embedding_local_only);
        assert!(s.scan.dirs.is_empty());
    }

    #[test]
    fn save_and_load_roundtrips() {
        let d = db();
        let mut s = Settings::default();
        s.llm.cloud_provider = CloudProvider::Qwen;
        s.llm.cloud_model = "qwen3-max".into();
        s.llm.route_deep = RouteTarget::Cloud;
        s.appearance.theme = Theme::Light;
        d.settings().save(&s).unwrap();

        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.llm.cloud_provider, CloudProvider::Qwen);
        assert_eq!(got.llm.cloud_model, "qwen3-max");
        assert_eq!(got.llm.route_deep, RouteTarget::Cloud);
        assert_eq!(got.appearance.theme, Theme::Light);
    }

    /// API Key 必须能存能取（否则连接测试永远失败）。
    #[test]
    fn api_key_persists_separately() {
        let d = db();
        let mut s = Settings::default();
        s.llm.cloud_api_key = "sk-secret-value-12345".into();
        d.settings().save(&s).unwrap();

        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.llm.cloud_api_key, "sk-secret-value-12345");
        assert_eq!(d.settings().get_api_key().unwrap(), "sk-secret-value-12345");
    }

    /// API Key 标记为敏感：导出诊断时不得泄漏。
    #[test]
    fn api_key_is_excluded_from_export() {
        let d = db();
        let mut s = Settings::default();
        s.llm.cloud_api_key = "sk-secret-value-12345".into();
        d.settings().save(&s).unwrap();

        let exported = d.settings().export_non_sensitive().unwrap();
        assert!(exported.iter().any(|(k, _)| k == keys::LLM));
        assert!(
            !exported.iter().any(|(k, _)| k == keys::API_KEY),
            "敏感键不得出现在导出中"
        );
        let joined: String = exported.iter().map(|(_, v)| v.clone()).collect();
        assert!(!joined.contains("sk-secret"), "明文 key 不得泄漏到导出");
    }

    /// LLM 配置的普通部分（非 key）序列化时不应含明文 key。
    #[test]
    fn llm_blob_does_not_contain_key() {
        let d = db();
        let mut s = Settings::default();
        s.llm.cloud_api_key = "sk-secret-value-12345".into();
        d.settings().save(&s).unwrap();
        let exported = d.settings().export_non_sensitive().unwrap();
        let llm_blob = exported.iter().find(|(k, _)| k == keys::LLM).unwrap();
        assert!(!llm_blob.1.contains("sk-secret"), "LLM blob 应已摘除 key");
    }

    #[test]
    fn scan_dirs_roundtrip() {
        let d = db();
        let mut s = Settings::default();
        s.scan.dirs.push(ScanDir {
            path: "D:/Projects".into(),
            enabled: true,
            added_at: now_utc(),
            last_scanned_at: None,
            project_count: None,
        });
        d.settings().save(&s).unwrap();
        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.scan.dirs.len(), 1);
        assert_eq!(got.scan.dirs[0].path, "D:/Projects");
        assert!(got.scan.dirs[0].enabled);
    }

    #[test]
    fn save_llm_only_touches_llm() {
        let d = db();
        let mut s = Settings::default();
        s.appearance.theme = Theme::Light;
        d.settings().save(&s).unwrap();

        let llm = LlmSettings {
            cloud_model: "gpt-5".into(),
            ..Default::default()
        };
        d.settings().save_llm(&llm).unwrap();

        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.llm.cloud_model, "gpt-5");
        assert_eq!(got.appearance.theme, Theme::Light, "外观设置不应被 llm 保存影响");
    }

    #[test]
    fn save_scan_and_appearance_independently() {
        let d = db();
        d.settings().save(&Settings::default()).unwrap();
        let mut scan = ScanSettings::default();
        scan.add_dir("E:/Code", "now");
        d.settings().save_scan(&scan).unwrap();
        let app = AppearanceSettings {
            reduce_motion: true,
            ..Default::default()
        };
        d.settings().save_appearance(&app).unwrap();

        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.scan.dirs.len(), 1);
        assert!(got.appearance.reduce_motion);
    }

    /// 🔴 回归守护：**只保存扫描目录、从未配置 LLM** 时，目录必须读得回来。
    ///
    /// 这是用户首次使用的真实路径（先加目录扫描，之后才配模型）。
    /// 旧实现用"LLM 行是否存在"判断首次运行，导致 `get_all` 返回 `None`，
    /// `get_or_default` 给出空默认值——用户刚添加的目录凭空消失，
    /// 扫描任务随即报"尚未添加任何扫描目录"，而设置页明明显示着那个目录。
    ///
    /// 注意与 `save_scan_and_appearance_independently` 的区别：
    /// 那个测试先调了 `save()`（会写入 LLM 行），恰好绕过了本缺陷。
    #[test]
    fn scan_dirs_survive_without_llm_config() {
        let d = db();
        // 不调用 save()：模拟用户只添加目录、从未配置大模型
        let mut scan = ScanSettings::default();
        assert!(scan.add_dir("F:/CodeProject", "now"));
        d.settings().save_scan(&scan).unwrap();

        // get_all 不应因 LLM 段缺失就判定"首次运行"
        let all = d.settings().get_all().unwrap();
        assert!(all.is_some(), "已保存过扫描设置，不应视为首次运行");
        let got = all.unwrap();
        assert_eq!(got.scan.dirs.len(), 1, "扫描目录不得丢失");
        assert_eq!(got.scan.dirs[0].path, "F:/CodeProject");
        // LLM 段缺失时应给出默认值，而不是让整个设置消失。
        // 断言具体字段而非整体相等（LlmSettings 无 PartialEq，
        // 且逐字段断言能直接指出是哪一项取了默认值）。
        assert!(
            got.llm.cloud_api_key.is_empty(),
            "未配置模型时 API Key 应为空"
        );
        assert_eq!(
            got.llm.cloud_model,
            LlmSettings::default().cloud_model,
            "模型名应取默认值"
        );

        // get_or_default 是扫描任务的真实入口，同样必须拿到目录
        let via_default = d.settings().get_or_default().unwrap();
        assert_eq!(
            via_default.scan.enabled_dirs().len(),
            1,
            "扫描任务通过 get_or_default 也必须看到授权目录"
        );
    }

    /// 只保存外观设置（同样不碰 LLM）也不该丢失。
    #[test]
    fn appearance_survives_without_llm_config() {
        let d = db();
        let app = AppearanceSettings {
            reduce_motion: true,
            ..Default::default()
        };
        d.settings().save_appearance(&app).unwrap();
        let got = d.settings().get_all().unwrap().unwrap();
        assert!(got.appearance.reduce_motion, "外观设置不得丢失");
    }

    /// 真正的首次运行（什么都没存）才返回 None。
    #[test]
    fn truly_empty_settings_returns_none() {
        let d = db();
        assert!(
            d.settings().get_all().unwrap().is_none(),
            "全新库应返回 None，让上层用默认值"
        );
    }

    #[test]
    fn save_api_key_directly() {
        let d = db();
        d.settings().save_api_key("sk-direct-key").unwrap();
        assert_eq!(d.settings().get_api_key().unwrap(), "sk-direct-key");
    }

    #[test]
    fn audit_roundtrips() {
        let d = db();
        d.settings().audit(&AuditEntry::llm_ok(
            now_utc(),
            "qwen3:8b",
            RouteTarget::Local,
            "ANALYZE_PROJECT",
            "分析 yingTech，发送 3 个关键模块",
            Some("p1".into()),
        )).unwrap();
        let logs = d.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].model, "qwen3:8b");
        assert_eq!(logs[0].route, RouteTarget::Local);
        assert_eq!(logs[0].project_id.as_deref(), Some("p1"));
        // 成功调用：ok=Some(true)，且必然没有错误原因
        assert_eq!(logs[0].ok, Some(true));
        assert_eq!(logs[0].error, None);
    }

    /// 🔴 `ok` 三态必须原样往返。
    ///
    /// NULL 与 0/1 是三种不同语义：`None` = 本地安全事件（不是调用），
    /// `Some(false)` = 调用失败。若读写把 NULL 压成 0，
    /// "用户关闭了安全约束"就会在 UI 上显示成一次**失败的红叉**；
    /// 压成 1 则显示成**绿色对勾**。两者都是主动误导。
    #[test]
    fn audit_ok_is_tristate_and_roundtrips() {
        let d = db();
        d.settings().audit(&AuditEntry::llm_ok(
            now_utc(), "m-ok", RouteTarget::Cloud, "ANALYZE_PROJECT", "成功", None,
        )).unwrap();
        d.settings().audit(&AuditEntry::llm_failed(
            now_utc(), "m-bad", RouteTarget::Cloud, "ANALYZE_PROJECT", "失败", None,
            "请求被拒绝 (400): The product is not activated",
        )).unwrap();
        d.settings().audit(&AuditEntry::event(
            now_utc(), "SETTINGS", "用户关闭了「敏感项目仅本地」约束", None,
        )).unwrap();

        // 按 at DESC, id DESC 排序，最后写入的事件在最前
        let logs = d.settings().recent_audit(10).unwrap();
        assert_eq!(logs.len(), 3);
        by_model(&logs, "m-ok", |e| {
            assert_eq!(e.ok, Some(true), "成功调用应为 Some(true)");
            assert_eq!(e.error, None);
        });
        by_model(&logs, "m-bad", |e| {
            assert_eq!(e.ok, Some(false), "失败调用应为 Some(false)");
            assert!(
                e.error.as_deref().unwrap_or("").contains("not activated"),
                "失败原因必须完整往返，实际 {:?}", e.error
            );
        });
        by_model(&logs, "-", |e| {
            assert_eq!(e.ok, None, "安全事件应为 None，绝不能被压成 true/false");
            assert_eq!(e.error, None);
            assert!(!e.is_llm_call(), "安全事件不是一次模型调用");
        });
    }

    /// `llm_failed` 收到空错误串时归一成 `None`，避免"失败但没原因"的半截记录。
    #[test]
    fn audit_blank_error_normalized_to_none() {
        let d = db();
        d.settings().audit(&AuditEntry::llm_failed(
            now_utc(), "m", RouteTarget::Cloud, "ANALYZE_PROJECT", "失败", None, "   ",
        )).unwrap();
        let logs = d.settings().recent_audit(10).unwrap();
        assert_eq!(logs[0].ok, Some(false), "仍应记为失败");
        assert_eq!(logs[0].error, None, "空白原因应归一成 NULL 而非存空串");
    }

    /// 按 model 找到那一条并断言（列表顺序不该是测试的关注点）。
    fn by_model(logs: &[AuditEntry], model: &str, assert_fn: impl Fn(&AuditEntry)) {
        let e = logs
            .iter()
            .find(|e| e.model == model)
            .unwrap_or_else(|| panic!("审计日志里找不到 model={model:?}，实际有 {:?}",
                logs.iter().map(|l| (&l.model, l.ok)).collect::<Vec<_>>()));
        assert_fn(e);
    }

    #[test]
    fn recent_audit_respects_limit() {
        let d = db();
        for i in 0..10 {
            d.settings().audit(&AuditEntry::llm_ok(
                now_utc(),
                format!("m{i}"),
                RouteTarget::Cloud,
                "GENERATE_INSIGHT",
                String::new(),
                None,
            )).unwrap();
        }
        assert_eq!(d.settings().recent_audit(3).unwrap().len(), 3);
    }

    /// 脏 JSON 不得让设置读取崩溃（降级为默认值）。
    #[test]
    fn corrupt_llm_blob_degrades_to_default() {
        let d = db();
        d.settings().save(&Settings::default()).unwrap();
        let conn = d.conn().unwrap();
        conn.execute("UPDATE settings SET value='{{bad' WHERE key='llm'", []).unwrap();
        drop(conn);
        let got = d.settings().get_all().unwrap().unwrap();
        // 降级为默认 LLM 设置，但整体仍可读
        assert_eq!(got.llm.cloud_provider, CloudProvider::OpenAiCompatible);
    }

    /// 只有 api_key、没有 llm 主配置时，仍应视为"未初始化"。
    #[test]
    fn api_key_without_llm_is_uninitialized() {
        let d = db();
        d.settings().save_api_key("sk-x").unwrap();
        assert!(d.settings().get_all().unwrap().is_none(), "缺 llm 主配置应视为首次运行");
        // 但 get_or_default 仍返回可用默认值 + 已存的 key？不——get_all 为 None 时用默认，key 丢失
        let s = d.settings().get_or_default().unwrap();
        assert!(s.llm.cloud_api_key.is_empty());
    }

    #[test]
    fn save_overwrites_previous() {
        let d = db();
        let mut s1 = Settings::default();
        s1.llm.cloud_model = "a".into();
        d.settings().save(&s1).unwrap();
        let mut s2 = Settings::default();
        s2.llm.cloud_model = "b".into();
        d.settings().save(&s2).unwrap();
        assert_eq!(d.settings().get_all().unwrap().unwrap().llm.cloud_model, "b");
    }

    #[test]
    fn local_backend_roundtrips() {
        let d = db();
        let mut s = Settings::default();
        s.llm.local_backend = LocalBackend::LmStudio;
        s.llm.local_model = "本地已加载模型".into();
        d.settings().save(&s).unwrap();
        let got = d.settings().get_all().unwrap().unwrap();
        assert_eq!(got.llm.local_backend, LocalBackend::LmStudio);
        assert_eq!(got.llm.local_model, "本地已加载模型");
    }
}
