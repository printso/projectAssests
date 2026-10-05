//! 本机目录浏览（设置页「选择目录」弹窗的数据源）。
//!
//! # 为什么需要它
//! 早期添加扫描目录只能**手输绝对路径**：用户要切到资源管理器、复制路径、
//! 回来粘贴，任何一步出错（少个斜杠、盘符大小写、中文路径打错）都会得到
//! 一句"目录不存在"。这是新用户 onboarding 的第一个卡点。
//! 本模块给前端弹窗提供"逐层浏览 + 勾选"的能力，手输降级为兜底。
//!
//! # 🔴 安全边界（Local-First 的延伸）
//! - **只读**：本模块不写任何文件，只列目录。
//! - **只监听回环**：服务默认绑 127.0.0.1（见 `apps/server`），
//!   局域网内其他机器无法借这个端点浏览你的磁盘。
//! - **不读文件内容**：只取目录名与少量元信息，绝不打开文件。
//! - **截断**：单层条目上限 [`MAX_ENTRIES`]，防止列 `C:\` 这类巨型目录
//!   时把响应撑爆、把前端列表卡死。截断时如实标记 `truncated`，
//!   前端据此提示"用搜索/手输定位"，而不是假装列全了。
//!
//! # 为什么不返回文件
//! 扫描目录的授权对象是**目录**。列出文件只会增加噪音与响应体积，
//! 还会诱导用户去选文件（选了我们也要拒绝，等于制造一次失败体验）。

use serde::Serialize;

use crate::context::ServiceError;

/// 单层最多返回的条目数。
///
/// 取 500 的理由：常见代码根目录（`D:/Projects`）一层通常几十个条目，
/// 500 足够覆盖；而 `C:\Users\xxx\AppData` 这类目录可能有上万条目，
/// 不截断会让一次点击卡住数秒并传输几 MB JSON。
pub const MAX_ENTRIES: usize = 500;

/// 单个目录条目（只含弹窗渲染所需的最少字段）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FsEntry {
    /// 目录名（不含父路径）
    pub name: String,
    /// 完整路径（前端勾选后直接提交给 add_dir）
    pub path: String,
    /// 是否还能往下进（无可读子目录时为 false，前端禁用展开）
    pub has_children: bool,
}

/// 一层目录的浏览结果。
#[derive(Debug, Clone, Serialize)]
pub struct FsListView {
    /// 本次列出的目录（规范化后的请求路径）
    pub path: String,
    /// 父目录；已到根则为 `None`（前端据此禁用「上一级」）
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
    /// 是否因超过 [`MAX_ENTRIES`] 被截断
    pub truncated: bool,
    /// 是否显示了隐藏目录（前端开关回显）
    pub show_hidden: bool,
    /// 快速入口（盘符 / 用户主目录等），前端渲染成面包屑旁的快捷按钮
    pub roots: Vec<FsEntry>,
}

/// 浏览一层目录。
///
/// `path` 为空串时表示"列出根"（Windows 为各盘符，Unix 为 `/`）。
/// 🔴 所有失败都带**可操作**的 message：路径不存在、不是目录、无权限
/// 是三种不同的用户处境，笼统报"失败"会让他反复盲试。
pub fn list_dir(path: &str, show_hidden: bool) -> Result<FsListView, ServiceError> {
    let roots = quick_roots();

    // 空路径 = 列根：Windows 下列盘符，Unix 下列 "/"
    if path.trim().is_empty() {
        return Ok(FsListView {
            path: String::new(),
            parent: None,
            entries: root_entries(show_hidden),
            truncated: false,
            show_hidden,
            roots,
        });
    }

    let p = std::path::Path::new(path.trim());
    if !p.exists() {
        return Err(ServiceError::Invalid(format!("目录不存在：{path}")));
    }
    if !p.is_dir() {
        return Err(ServiceError::Invalid(format!("不是目录：{path}")));
    }

    let read = p.read_dir().map_err(|e| {
        ServiceError::Precondition(format!("无法读取目录 {path}：{e}（检查权限）"))
    })?;

    let mut dirs: Vec<FsEntry> = Vec::new();
    for item in read.flatten() {
        let ft = match item.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().to_string();
        let full = item.path();
        if !show_hidden && is_hidden(&full, &name) {
            continue;
        }
        dirs.push(FsEntry {
            has_children: has_readable_subdir(&full),
            name,
            // 🔴 统一成正斜杠：Windows 的 PathBuf 会拼出 `F:/a\b` 这种
            // 混排路径，面包屑与列表里非常难看；Windows API 全接受 `/`。
            path: normalize_slashes(&full.to_string_lossy()),
        });
    }
    // 稳定排序：不依赖文件系统返回顺序，同一目录两次打开列表一致。
    // 按小写名排，避免大写字母全部排在前面（Windows 上 "Zebra" 会排在 "apple" 前）。
    // 🔴 用 `sort_by_cached_key` 而非 clippy 建议的 `sort_by_key`：
    // 小写化会**分配 String**，`sort_by_key` 在每次比较时都重算一遍（O(n log n) 次分配），
    // 而目录列表可能上千条。cached 版每个元素只算一次 key（O(n)）。
    dirs.sort_by_cached_key(|e| e.name.to_lowercase());

    let truncated = dirs.len() > MAX_ENTRIES;
    if truncated {
        dirs.truncate(MAX_ENTRIES);
    }

    Ok(FsListView {
        parent: parent_of(p),
        path: normalize_slashes(&p.to_string_lossy()),
        entries: dirs,
        truncated,
        show_hidden,
        roots,
    })
}

/// 路径展示归一化：反斜杠转正斜杠；盘符根补尾斜杠（`C:` → `C:/`）。
///
/// 只用于**展示与回传**，不改变文件系统语义（Windows 两者都接受）。
fn normalize_slashes(s: &str) -> String {
    let mut out = s.replace('\\', "/");
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    // 盘符根：`C:` → `C:/`，保证 join 语义与面包屑一致
    if out.len() == 2 && out.ends_with(':') {
        out.push('/');
    }
    out
}

/// 根条目：Windows 为各盘符（`C:\`…），Unix 为 `/`。
fn root_entries(show_hidden: bool) -> Vec<FsEntry> {
    #[cfg(windows)]
    {
        // 不引入额外 crate：用 Windows API 的逻辑等价物——遍历 A..Z 判断盘符存在。
        let mut out = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let p = std::path::Path::new(&root);
            if !p.is_dir() {
                continue;
            }
            out.push(FsEntry {
                has_children: true,
                name: format!("{}:", letter as char),
                path: normalize_slashes(&root),
            });
        }
        let _ = show_hidden; // 根层没有隐藏概念
        out
    }
    #[cfg(not(windows))]
    {
        let _ = show_hidden;
        vec![FsEntry {
            name: "/".to_string(),
            path: "/".to_string(),
            has_children: true,
        }]
    }
}

/// 快速入口：用户主目录、桌面、文档、下载、当前工作目录。
///
/// 🔴 只给**大概率存在**的目录：不存在的直接过滤掉，
/// 前端不该出现点了就报错的快捷按钮。
fn quick_roots() -> Vec<FsEntry> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = std::path::PathBuf::from(home);
        push_root(&mut out, &home, "主目录");
        for sub in ["Desktop", "Documents", "Downloads", "桌面", "文档", "下载"] {
            push_root(&mut out, &home.join(sub), sub);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        push_root(&mut out, &cwd, "当前工作目录");
    }
    // 去重（主目录与 cwd 可能相同）
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn push_root(out: &mut Vec<FsEntry>, p: &std::path::Path, label: &str) {
    if !p.is_dir() {
        return;
    }
    let path = normalize_slashes(&p.to_string_lossy());
    if out.iter().any(|e| e.path == path) {
        return;
    }
    out.push(FsEntry {
        name: label.to_string(),
        path,
        has_children: true,
    });
}

/// 父目录；盘符根（`C:\`）与 `/` 的父为 `None`（再往上没有意义）。
fn parent_of(p: &std::path::Path) -> Option<String> {
    let parent = p.parent()?;
    let ps = parent.to_string_lossy().to_string();
    let cs = p.to_string_lossy().to_string();
    if ps == cs {
        return None;
    }
    if ps.is_empty() {
        return None;
    }
    Some(normalize_slashes(&ps))
}

/// 是否存在至少一个可读子目录（决定前端能否展开）。
///
/// 只探第一个匹配项就返回：这是**点击热路径**，
/// 对每个条目做全量 read_dir 会让大目录的浏览明显卡顿。
fn has_readable_subdir(p: &std::path::Path) -> bool {
    let Ok(rd) = p.read_dir() else {
        return false;
    };
    for item in rd.flatten() {
        if item.path().is_dir() {
            return true;
        }
    }
    false
}

/// 隐藏目录判定：Unix 以 `.` 开头；Windows 读 FILE_ATTRIBUTE_HIDDEN。
///
/// 🔴 Windows 上 `node_modules` 之类不算隐藏（用户可能真要扫），
/// 只有系统标记的隐藏目录才默认折叠——与资源管理器默认视图一致，
/// 用户的心智模型是"和资源管理器看到的一样"。
fn is_hidden(path: &std::path::Path, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        path.metadata()
            .map(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_subdirs_of_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("beta")).unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::write(dir.path().join("note.txt"), "x").unwrap();

        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        // 只列目录，不含文件
        assert_eq!(v.entries.len(), 2);
        // 排序稳定（大小写不敏感）
        assert_eq!(v.entries[0].name, "alpha");
        assert_eq!(v.entries[1].name, "beta");
        // 叶子目录没有子目录
        assert!(!v.entries[0].has_children);
        assert!(!v.truncated);
        assert!(v.parent.is_some());
        assert!(!v.roots.is_empty(), "应给快速入口");
    }

    #[test]
    fn hidden_dirs_are_filtered_unless_requested() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        std::fs::create_dir(dir.path().join("visible")).unwrap();

        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].name, "visible");

        let v2 = list_dir(&dir.path().to_string_lossy(), true).unwrap();
        assert_eq!(v2.entries.len(), 2);
        assert!(v2.show_hidden);
    }

    #[test]
    fn nested_dir_reports_has_children() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        assert!(v.entries[0].has_children, "a 下有 b，应可展开");
    }

    #[test]
    fn missing_and_file_paths_are_rejected() {
        let err = list_dir("C:/definitely-not-here-spolia", false).unwrap_err();
        assert!(matches!(err, ServiceError::Invalid(_)));
        assert!(err.to_string().contains("不存在"));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let err2 = list_dir(&file.to_string_lossy(), false).unwrap_err();
        assert!(err2.to_string().contains("不是目录"));
    }

    #[test]
    fn empty_path_lists_roots() {
        let v = list_dir("", false).unwrap();
        assert!(v.parent.is_none());
        // Windows 至少有一个盘符；Unix 有 "/"
        assert!(!v.entries.is_empty());
        #[cfg(windows)]
        assert!(
            v.entries.iter().all(|e| e.path.ends_with('/')),
            "盘符根应归一化为 C:/ 形式: {:?}",
            v.entries
        );
    }

    /// 🔴 Windows 上 PathBuf 会拼出 `F:/a\b` 混排路径，
    /// 面包屑里非常难看；返回给前端前必须统一成正斜杠。
    #[test]
    fn entry_paths_use_forward_slashes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        for e in &v.entries {
            assert!(!e.path.contains('\\'), "路径含反斜杠: {}", e.path);
        }
        assert!(!v.path.contains('\\'));
    }

    #[test]
    fn normalize_slashes_handles_drive_root_and_trailing() {
        assert_eq!(normalize_slashes("C:\\"), "C:/");
        assert_eq!(normalize_slashes("C:"), "C:/");
        assert_eq!(normalize_slashes("F:/a\\b/"), "F:/a/b");
        assert_eq!(normalize_slashes("/tmp/x//"), "/tmp/x");
    }

    #[test]
    fn truncation_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRIES + 5) {
            std::fs::create_dir(dir.path().join(format!("d{i:04}"))).unwrap();
        }
        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        assert!(v.truncated, "超出上限必须标记截断");
        assert_eq!(v.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn parent_of_root_is_none() {
        #[cfg(windows)]
        assert_eq!(parent_of(std::path::Path::new("C:\\")), None);
        #[cfg(not(windows))]
        assert_eq!(parent_of(std::path::Path::new("/")), None);
    }

    /// 🔴 排序必须大小写不敏感，且**稳定**（不依赖文件系统返回顺序）。
    ///
    /// 按字节序排的话大写字母全部沉到前面：`Zebra` 会排在 `apple` 之前，
    /// 用户在目录选择弹窗里看到的顺序与他熟悉的资源管理器完全不同——
    /// 而"和资源管理器看到的一样"正是这个弹窗的设计前提（见 `is_hidden` 的注释）。
    #[test]
    fn entries_sort_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        // 刻意乱序创建，且混排大小写：文件系统返回顺序不可依赖
        for name in ["Zebra", "apple", "banana", "Apricot"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        let v = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        let names: Vec<&str> = v.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["apple", "Apricot", "banana", "Zebra"],
            "应按小写名排序（Zebra 不该因首字母大写而排到最前）"
        );

        // 两次列举顺序必须一致（前端可能重复打开同一目录）
        let v2 = list_dir(&dir.path().to_string_lossy(), false).unwrap();
        let names2: Vec<&str> = v2.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, names2, "同一目录两次列举顺序必须一致");
    }
}
