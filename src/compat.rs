//! Other mods worth knowing about. The game lists the enabled mods in
//! `<game>/config/game/mods.json` (`enabled_mods`, by mod id); the list is read once at start.
//!
//! - tfm2mods' **Champion Position Lock** (and flover's rework, same id `tfm2_champ_pos_lock`)
//!   locks positions too, from lists the player fills in. Both at once stack: a champion has to
//!   pass both. The log says so; nothing is switched off.
//! - **Patch Meta AI** (`patch_meta_ai`) reads this mod's locks on the ban/pick screen and does
//!   not advise a locked card. Its earlier versions kept a `positions.json` of their own, which
//!   is taken over once when this mod has none yet (see `lanes`).

use std::path::PathBuf;

use serde_json::Value;

use crate::{diag, paths};

pub const OTHER_LOCK: &str = "tfm2_champ_pos_lock";
pub const PATCH_META_AI: &str = "patch_meta_ai";

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
}
