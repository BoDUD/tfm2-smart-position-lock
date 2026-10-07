//! Smart Position Lock - a team only picks champions that can still take one of its open
//! positions (Teamfight Manager 2, stable mod API, game 0.6+). Nothing to set up:
//!
//! - a champion's positions are its two **main positions** as the game shows them on its
//!   ban/pick card (what a champion mod set for it), learned on the ban/pick screen and kept in
//!   `positions.json`, plus every position it has **really played** in this save ([`history`]);
//! - a pick is legal when the team's picks and the candidate can still be seated one per
//!   position ([`lanes`]); when nothing on offer is legal, everything is - a draft never gets
//!   stuck. Bans are never restricted;
//! - the AI's picks are held to it by a draft score hook ([`hook`]), the player's own picks by a
//!   lock over each card that does not fit, which takes the click ([`screen`]).
//!
//! Nothing in a callback may panic: every lock tolerates poisoning and lists are never indexed
//! with positions from an earlier call.

mod client;
pub mod compat;
pub mod config;
mod diag;
pub mod history;
pub mod hook;
pub mod lanes;
pub mod panel;
mod paths;
pub mod screen;
pub mod ui;

use mod_api_stable::{declare_stable_mod, LogLevel, StableHost, StableMod};

/// Mod id = folder name = DLL file name. Never changes after release.
pub const MOD_ID: &str = "smart_position_lock";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn init(host: &StableHost) -> StableMod {
    let game = host.game_version();
    let dir = paths::mod_dir();
    diag::open(&dir);
    let header = format!(
        "{MOD_ID} {VERSION} loaded (game {}.{}.{}, host ABI level {}, folder {})",
        game.major,
        game.minor,
        game.patch,
        host.abi_level(),
        dir.display()
    );
    diag::log(&header);
    host.log(LogLevel::Info, &header);
    compat::load();
    config::load_now();
    lanes::load();

    let mut decl = StableMod::new(MOD_ID);
    decl.set_extension(client::ClientExt);
    decl.add_draft_score_hook(hook::LockHook);
    decl
}

declare_stable_mod!(init);

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    static SERIAL: Mutex<()> = Mutex::new(());

    /// Tests that touch the process-wide state run one at a time.
    pub fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A folder of its own for the files a test writes.
    pub fn temp_dir() {
        // a mods folder of its own: the old Patch Meta AI file is looked for next to this one
        let dir = std::env::temp_dir().join(format!("spl_test_{}", std::process::id())).join(crate::MOD_ID);
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var(crate::paths::DIR_ENV, &dir);
        crate::diag::open(&dir);
    }
}
