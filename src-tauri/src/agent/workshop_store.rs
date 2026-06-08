//! ③/⑥ 工作室定义（Studio Definition）的存储。
//!
//! 一个工作室 = `workshop.json`（名称/图标/工作区间 + 一组 `RoleConfig`），落在
//! `~/.aidock/users/{user}/workshops/{ws}/workshop.json`（见 `session_store` 布局）。
//! 这是"定义 vs 会话"里的**定义**（Class），可被会话复用、将来可上架市场
//! （详见 `docs/WORKSHOP_EDITOR_DESIGN.md`、`AIDOCK_DESIGN §3.2/§5`）。
//!
//! `default_workshop()`（写死的 PM+前端+后端）从"运行时来源"退成"**新建工作室的
//! 种子模板**"：首次 [`load_workshop`] 发现没有 `workshop.json` 时用它建种并落盘，
//! 现有 `ws-default` 因此自动迁移、行为不变。

use serde::{Deserialize, Serialize};

use crate::agent::persistence::{self, PersistError};
use crate::agent::role::RoleConfig;
use crate::agent::roles::{default_seed_role, default_workshop};
use std::path::PathBuf;

use crate::agent::session_store::{
    default_workspace_dir, new_workshop_id, workshop_dir, workshops_root, DEFAULT_WORKSHOP,
};

/// 一个工作室的完整定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkshopDef {
    /// 与目录名一致的工作室 id。
    pub id: String,
    /// 展示名（用户可改）。
    pub name: String,
    /// 展示图标（emoji/字形）。
    #[serde(default)]
    pub icon: String,
    /// ④.d 可信工作区间。**P1 暂不接线**——为空时运行时仍回落"每会话 workspace 子
    /// 目录"（现行为）。P3 接 UI 选择器后用它当全室共享的 confinement 边界。
    #[serde(default)]
    pub workspace_path: String,
    /// 工作室成员（顺序即展示顺序）。为空时 [`load_workshop`] 用最新默认重填
    /// （用于"重置角色到默认"——把 roles 清空即可，name/icon/workspace 保留）。
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
}

impl WorkshopDef {
    /// 增/改一个角色（按 id upsert）。强制**协调者全室唯一**：若新角色是协调者，
    /// 其余角色取消协调者；改完保证至少有一个协调者。
    pub fn upsert_role(&mut self, role: RoleConfig) {
        if role.is_coordinator {
            for r in &mut self.roles {
                if r.id != role.id {
                    r.is_coordinator = false;
                }
            }
        }
        match self.roles.iter_mut().find(|r| r.id == role.id) {
            Some(existing) => *existing = role,
            None => self.roles.push(role),
        }
        self.ensure_coordinator();
    }

    /// 删一个角色：清掉其他角色 teammates 里对它的引用，保证仍有协调者。
    /// 拒绝删到空工作室（至少留一个角色）。
    pub fn remove_role(&mut self, role_id: &str) -> Result<(), &'static str> {
        if self.roles.len() <= 1 {
            return Err("工作室至少保留一个角色");
        }
        if !self.roles.iter().any(|r| r.id == role_id) {
            return Err("角色不存在");
        }
        self.roles.retain(|r| r.id != role_id);
        for r in &mut self.roles {
            r.teammates.retain(|t| t.as_str() != role_id);
        }
        self.ensure_coordinator();
        Ok(())
    }

    /// 按显示名生成一个**唯一**角色 id（用户不用手填）。英文名 → slug（小写、非字母
    /// 数字转 `_`）；中文/空名 → `role`。冲突则加 `_2`/`_3`…
    pub fn gen_role_id(&self, display_name: &str) -> String {
        let mut base: String = display_name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        while base.contains("__") {
            base = base.replace("__", "_");
        }
        let base = base.trim_matches('_');
        let base = if base.is_empty() { "role" } else { base };
        if !self.roles.iter().any(|r| r.id == base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|cand| !self.roles.iter().any(|r| r.id == *cand))
            .unwrap_or_else(|| base.to_string())
    }

    /// 没有任何协调者时，把第一个角色升为协调者（全室必有一个对接用户的）。
    fn ensure_coordinator(&mut self) {
        if !self.roles.is_empty() && !self.roles.iter().any(|r| r.is_coordinator) {
            self.roles[0].is_coordinator = true;
        }
    }
}

fn workshop_json_path(user: &str, ws: &str) -> std::path::PathBuf {
    workshop_dir(user, ws).join("workshop.json")
}

/// 读工作室定义；没有 `workshop.json` 就用 [`default_workshop`] 做种子写盘并返回
/// （首次迁移）。读到损坏文件时同样回落种子——保证永远能拿到一个可用定义。
pub fn load_workshop(user: &str, ws: &str) -> WorkshopDef {
    let path = workshop_json_path(user, ws);
    if let Ok(Some(mut def)) = persistence::read_json::<WorkshopDef>(&path) {
        // roles 被清空（重置/迁移）→ 用最新默认重填，保留 name/icon/workspace_path。
        if def.roles.is_empty() {
            def.roles = default_workshop();
            let _ = save_workshop(user, ws, &def);
        }
        return def;
    }
    let def = seed_workshop(ws);
    // 写盘失败不致命：本次仍返回种子，下次再尝试落盘。
    let _ = save_workshop(user, ws, &def);
    def
}

/// 覆盖写工作室定义（原子写，自动建父目录）。
pub fn save_workshop(user: &str, ws: &str, def: &WorkshopDef) -> Result<(), PersistError> {
    persistence::atomic_write_json(&workshop_json_path(user, ws), def)
}

/// 用写死的默认三人组（PM/前端/后端）建一个种子定义。
pub fn seed_workshop(ws: &str) -> WorkshopDef {
    WorkshopDef {
        id: ws.to_string(),
        name: "默认工作室".to_string(),
        icon: "🧭".to_string(),
        workspace_path: String::new(),
        roles: default_workshop(),
    }
}

/// 工作台卡片墙用的轻量摘要（不带 roles 全量）。
#[derive(Debug, Clone, Serialize)]
pub struct WorkshopMeta {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub member_count: usize,
    /// 用户指定的自定义工作区间（可空——空=用默认）。
    pub workspace_path: String,
    /// 默认工作区间路径（`{workshop}/workspace`），给设置面板显示"未设时用它"。
    pub default_workspace: String,
}

impl WorkshopMeta {
    fn from_def(user: &str, def: &WorkshopDef) -> Self {
        Self {
            id: def.id.clone(),
            name: def.name.clone(),
            icon: def.icon.clone(),
            member_count: def.roles.len(),
            workspace_path: def.workspace_path.clone(),
            default_workspace: default_workspace_dir(user, &def.id)
                .to_string_lossy()
                .into_owned(),
        }
    }
}

/// 一个工作室**实际生效**的工作区间（④.d confinement 根）：设置了自定义路径就用它，
/// 否则用默认 `{workshop}/workspace`。`Session::start` 据此约束 agent 的改/删。
pub fn effective_workspace(user: &str, ws: &str, def: &WorkshopDef) -> PathBuf {
    let wp = def.workspace_path.trim();
    if wp.is_empty() {
        default_workspace_dir(user, ws)
    } else {
        PathBuf::from(wp)
    }
}

/// 列出一个用户的全部工作室（扫 `workshops/` 子目录，每个 `load_workshop`）。
/// 若一个都没有，先种出默认工作室再返回——保证工作台永远有得选。
pub fn list_workshops(user: &str) -> Vec<WorkshopMeta> {
    let root = workshops_root(user);
    let mut out: Vec<WorkshopMeta> = match std::fs::read_dir(&root) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .map(|id| WorkshopMeta::from_def(user, &load_workshop(user, &id)))
            .collect(),
        Err(_) => Vec::new(),
    };
    if out.is_empty() {
        out.push(WorkshopMeta::from_def(
            user,
            &load_workshop(user, DEFAULT_WORKSHOP),
        ));
    }
    // 创建时间在 id 前缀里（ws-{millis}_…），按它倒序——新建的在前；ws-default 垫底。
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// 新建工作室：只放**一个默认协调者角色**（对接用户、可改 persona），用户再自行
/// 添加成员组队——不再默认塞三人组。
pub fn create_workshop(user: &str, name: &str, icon: &str) -> Result<WorkshopMeta, PersistError> {
    let id = new_workshop_id();
    let def = WorkshopDef {
        id: id.clone(),
        name: if name.trim().is_empty() {
            "新工作室".to_string()
        } else {
            name.trim().to_string()
        },
        icon: if icon.trim().is_empty() {
            "🛠".to_string()
        } else {
            icon.trim().to_string()
        },
        workspace_path: String::new(),
        roles: vec![default_seed_role()],
    };
    save_workshop(user, &id, &def)?;
    Ok(WorkshopMeta::from_def(user, &def))
}

/// 删除一个工作室（连其会话一起）。拒删最后一个（保证至少留一个）。
pub fn delete_workshop(user: &str, ws: &str) -> Result<(), String> {
    if list_workshops(user).len() <= 1 {
        return Err("至少保留一个工作室".to_string());
    }
    std::fs::remove_dir_all(workshop_dir(user, ws)).map_err(|e| format!("删除失败: {e}"))
}

/// 改工作室设置（名称/图标/工作区间）。`workspace_path` 非空时校验为存在的目录。
pub fn update_settings(
    user: &str,
    ws: &str,
    name: &str,
    icon: &str,
    workspace_path: &str,
) -> Result<(), String> {
    let wp = workspace_path.trim();
    if !wp.is_empty() && !std::path::Path::new(wp).is_dir() {
        return Err(format!("工作区间不是有效目录: {wp}"));
    }
    let mut def = load_workshop(user, ws);
    if !name.trim().is_empty() {
        def.name = name.trim().to_string();
    }
    if !icon.trim().is_empty() {
        def.icon = icon.trim().to_string();
    }
    def.workspace_path = wp.to_string();
    save_workshop(user, ws, &def).map_err(|e| format!("保存失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmp_user_ws() -> (String, String) {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        (format!("wstest-{}-{n}", std::process::id()), "ws-test".to_string())
    }

    #[test]
    fn load_seeds_then_persists_and_reloads() {
        let (user, ws) = tmp_user_ws();
        // 没有 workshop.json → 种子，并落盘。
        let first = load_workshop(&user, &ws);
        assert_eq!(first.id, ws);
        assert!(first.roles.iter().any(|r| r.is_coordinator), "种子里得有协调者");
        assert!(workshop_dir(&user, &ws).join("workshop.json").is_file());

        // 二次加载读的是落盘文件（角色数一致）。
        let again = load_workshop(&user, &ws);
        assert_eq!(again.roles.len(), first.roles.len());

        // 清理。
        let _ = std::fs::remove_dir_all(workshop_dir(&user, &ws));
    }

    #[test]
    fn create_workshop_seeds_single_coordinator() {
        let (user, _) = tmp_user_ws();
        let meta = create_workshop(&user, "我的工作室", "🚀").unwrap();
        assert_eq!(meta.member_count, 1, "新工作室只给一个默认角色");
        let def = load_workshop(&user, &meta.id);
        assert_eq!(def.roles.len(), 1);
        assert!(def.roles[0].is_coordinator, "唯一的默认角色必须是协调者");
        let _ = std::fs::remove_dir_all(workshop_dir(&user, &meta.id));
    }

    #[test]
    fn gen_role_id_auto_and_unique() {
        let mut def = seed_workshop("ws-q");
        assert_eq!(def.gen_role_id("Tester"), "tester");
        assert_eq!(def.gen_role_id("测试工程师"), "role"); // 无 ascii → role
        let mut t = default_seed_role();
        t.id = "tester".to_string();
        def.upsert_role(t);
        assert_eq!(def.gen_role_id("Tester"), "tester_2"); // 冲突去重
    }

    #[test]
    fn upsert_role_enforces_single_coordinator() {
        let mut def = seed_workshop("ws-x"); // PM(协调者) + 前端 + 后端
        let mut fe = def.roles.iter().find(|r| r.id == "frontend_dev").unwrap().clone();
        // 把前端设成协调者 → PM 应被取消，全室仍唯一一个协调者。
        fe.is_coordinator = true;
        def.upsert_role(fe);
        let coords: Vec<&str> = def
            .roles
            .iter()
            .filter(|r| r.is_coordinator)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(coords, vec!["frontend_dev"]);
    }

    #[test]
    fn remove_role_cleans_teammates_and_keeps_coordinator() {
        let mut def = seed_workshop("ws-y");
        def.remove_role("backend_dev").unwrap();
        assert!(!def.roles.iter().any(|r| r.id == "backend_dev"));
        // 其他角色的 teammates 不再引用被删的 backend_dev。
        assert!(def
            .roles
            .iter()
            .all(|r| !r.teammates.iter().any(|t| t == "backend_dev")));
        // 仍有协调者。
        assert!(def.roles.iter().any(|r| r.is_coordinator));
    }

    #[test]
    fn remove_role_reassigns_coordinator_then_refuses_last() {
        let mut def = seed_workshop("ws-z");
        def.remove_role("PM").unwrap(); // 删的是协调者 → 自动改派给第一个
        assert!(def.roles.iter().any(|r| r.is_coordinator));
        // 删到只剩一个后，再删被拒。
        def.remove_role(&def.roles[1].id.clone()).unwrap();
        assert_eq!(def.roles.len(), 1);
        assert!(def.remove_role(&def.roles[0].id.clone()).is_err());
    }

    #[test]
    fn save_overwrites_and_roundtrips_edits() {
        let (user, ws) = tmp_user_ws();
        let mut def = load_workshop(&user, &ws);
        def.name = "我的代码工作室".to_string();
        def.roles.truncate(1); // 留一个角色，验证编辑落盘
        save_workshop(&user, &ws, &def).unwrap();

        let reloaded = load_workshop(&user, &ws);
        assert_eq!(reloaded.name, "我的代码工作室");
        assert_eq!(reloaded.roles.len(), 1);

        let _ = std::fs::remove_dir_all(workshop_dir(&user, &ws));
    }
}
