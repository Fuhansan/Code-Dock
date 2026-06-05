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
use crate::agent::roles::default_workshop;
use crate::agent::session_store::workshop_dir;

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
    /// 工作室成员（顺序即展示顺序）。
    pub roles: Vec<RoleConfig>,
}

fn workshop_json_path(user: &str, ws: &str) -> std::path::PathBuf {
    workshop_dir(user, ws).join("workshop.json")
}

/// 读工作室定义；没有 `workshop.json` 就用 [`default_workshop`] 做种子写盘并返回
/// （首次迁移）。读到损坏文件时同样回落种子——保证永远能拿到一个可用定义。
pub fn load_workshop(user: &str, ws: &str) -> WorkshopDef {
    let path = workshop_json_path(user, ws);
    if let Ok(Some(def)) = persistence::read_json::<WorkshopDef>(&path) {
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
