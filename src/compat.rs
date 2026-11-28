//! Other mods worth knowing about. The game lists the enabled mods in
//! `<game>/config/game/mods.json` (`enabled_mods`, by mod id); the list is read once at start.
//!
//! - tfm2mods' **Champion Position Lock** (and flover's rework, same id `tfm2_champ_pos_lock`)
//!   locks positions too, from lists the player fills in. Both at once stack: a champion has to
//!   pass both. The log says so; nothing is switched off.
//! - **Patch Meta AI** (`patch_meta_ai`) reads this mod's locks on the ban/pick screen and does
//!   not advise a locked card. Its earlier versions kept a `positions.json` of their own, which
//!   is taken over once when this mod has none yet (see `lanes`).
//! - 海克斯大乱斗（`league_hextech`）把死斗生涯改成了嚎哭深渊：五对五一条路，没有分路。打开的是这种生涯时
//!   [`aram`] 为真，锁位置整个不管（1.3.79）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::{diag, paths};

pub const OTHER_LOCK: &str = "tfm2_champ_pos_lock";
pub const PATCH_META_AI: &str = "patch_meta_ai";
/// 海克斯大乱斗的客户端每帧看一眼打开的生涯，把这个进程环境变量写成 `1`（嚎哭深渊）或 `0`（不是）；
/// 没有这个变量 = 没装它，一切照旧。
pub const HEXTECH_ARAM_ENV: &str = "TFM2_HEXTECH_ARAM";

/// 现在打开的是不是嚎哭深渊生涯：是的话不锁选人（AI 的和你自己的都不锁）、不给 AI 队分路、不写 seated.json。
/// 每次用到时现读环境变量（同一个进程里谁写的都读得到，和加载顺序无关），变了在 diag.log 记一行。
pub fn aram() -> bool {
    static LAST: AtomicBool = AtomicBool::new(false);
    let on = is_aram(std::env::var(HEXTECH_ARAM_ENV).ok().as_deref());
    if LAST.swap(on, Ordering::Relaxed) != on {
        diag::log(if on {
            "Howling Abyss career (league_hextech: one lane, no positions): picks are not held, \
             the game seats AI teams itself, no seated.json"
        } else {
            "not a Howling Abyss career any more: positions are held again"
        });
    }
    on
}

/// 只有 `1` 算嚎哭深渊
fn is_aram(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// Enabled mod ids, from `mods.json` text.
pub fn enabled_mods(text: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("enabled_mods")?
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

/// `<game>/config/game/mods.json`, found from the game executable.
fn mods_json() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(paths::MODS_JSON_ENV) {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("config").join("game").join("mods.json"))
}

/// Logs the other mods that matter here.
pub fn load() {
    let Some(enabled) = mods_json().and_then(|p| std::fs::read(p).ok()).and_then(|b| enabled_mods(&String::from_utf8_lossy(&b)))
    else {
        diag::log("mods.json not readable: other mods unknown");
        return;
    };
    if enabled.iter().any(|m| m == OTHER_LOCK) {
        diag::log(
            "\"Champion Position Lock\" is enabled too: a champion has to pass both locks. \
             Disable one of them if picks look too narrow.",
        );
    }
    if enabled.iter().any(|m| m == PATCH_META_AI) {
        diag::log("Patch Meta AI is enabled: it skips the cards this mod locks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_enabled_list() {
        assert_eq!(enabled_mods(r#"{"enabled_mods":["base","patch_meta_ai"]}"#), Some(vec!["base".into(), "patch_meta_ai".into()]));
        assert_eq!(enabled_mods("{}"), None);
    }

    #[test]
    fn howling_abyss_from_league_hextech() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        assert!(!is_aram(None), "没装海克斯大乱斗");
        assert!(!is_aram(Some("0")) && is_aram(Some("1")) && !is_aram(Some("")));
        assert!(!aram());
        {
            let _abyss = crate::tests::HowlingAbyss::on();
            assert!(aram());
            std::env::set_var(HEXTECH_ARAM_ENV, "0");
            assert!(!aram());
            std::env::set_var(HEXTECH_ARAM_ENV, "1");
            assert!(aram() && aram());
        }
        assert!(!aram(), "变量去掉了");
        // 变一次记一行
        let log = std::fs::read_to_string(paths::mod_dir().join(diag::LOG_FILE)).unwrap_or_default();
        assert_eq!(log.matches("] Howling Abyss career").count(), 2, "{log}");
        assert_eq!(log.matches("not a Howling Abyss career any more").count(), 2, "{log}");
    }
}
