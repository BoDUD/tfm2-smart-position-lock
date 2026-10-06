//! `settings.ini` in the mod folder. Written with the defaults on first run; changes are picked
//! up within a few seconds, no restart needed. Unknown keys and bad values are logged and
//! ignored.
//!
//! `[positions]` overrides what the mod works out for a champion: `ahri=Mid,Support` (the
//! champion id as the game names it, any case). An override is used as it is - the champion's
//! main positions and history are not added to it.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::lanes::{parse_role, Lanes, Rules};
use crate::{diag, paths};

pub const FILE: &str = "settings.ini";
const CHECK_EVERY: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The whole lock.
    pub enabled: bool,
    /// The AI teams' picks.
    pub ai: bool,
    /// The player's own picks on the ban/pick screen.
    pub player: bool,
    /// Positions from this save's matches.
    pub history: bool,
    pub min_games: u32,
    pub share: f32,
    /// By lower-case champion id.
    pub overrides: HashMap<String, Lanes>,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: true, ai: true, player: true, history: true, min_games: 8, share: 0.15, overrides: HashMap::new() }
    }
}

impl Config {
    pub fn rules(&self) -> Rules {
        Rules { min_games: if self.history { self.min_games } else { u32::MAX }, share: self.share }
    }

    pub fn override_of(&self, champion: &str) -> Option<Lanes> {
        self.overrides.get(&champion.to_lowercase()).copied()
    }
}

const TEMPLATE: &str = "\
; Smart Position Lock - settings. Saved changes apply within a few seconds.
; 智能位置锁定 - 设置。保存后几秒内生效。

[lock]
; on / off: the whole lock (总开关)
enabled=on
; the AI teams' picks (AI 选人)
ai=on
; your own picks on the ban/pick screen: cards that do not fit are covered by a lock (你自己选人)
player=on

[history]
; a champion's positions are its two main positions on its ban/pick card, plus every position it
; has really played in this save: once it has min_games games, a position with at least `share`
; of them counts. history=off uses the main positions only.
; 英雄能打的位置 = 选人卡片上的两个主位置 + 本存档里实际打过的位置（至少 min_games 场、占比至少 share）
history=on
min_games=8
share=0.15

[positions]
; your own positions for a champion, used instead of everything above (champion id = position list)
; 手动指定某个英雄的位置（英雄 id = 位置列表），会完全替代自动判断，例如：
; ahri=Mid,Support
";

static CURRENT: RwLock<Option<Arc<Config>>> = RwLock::new(None);

struct Watch {
    stamp: Option<(Option<SystemTime>, u64)>,
    next_check: Option<Instant>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { stamp: None, next_check: None });

pub fn get() -> Arc<Config> {
    match CURRENT.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
        Some(cfg) => cfg.clone(),
        None => Arc::new(Config::default()),
    }
}

fn path() -> PathBuf {
    paths::mod_dir().join(FILE)
}

fn stamp(path: &PathBuf) -> Option<(Option<SystemTime>, u64)> {
    fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

/// Reads the settings now (writing the template first if the file does not exist).
pub fn load_now() {
    let path = path();
    if !path.exists() {
        match fs::write(&path, TEMPLATE) {
            Ok(()) => diag::log(&format!("wrote default {}", path.display())),
            Err(err) => diag::log(&format!("cannot write {} ({err}); using built-in defaults", path.display())),
        }
    }
    let mut watch = WATCH.lock().unwrap_or_else(PoisonError::into_inner);
    watch.stamp = stamp(&path);
    watch.next_check = Some(Instant::now() + CHECK_EVERY);
    drop(watch);
    reload(&path);
}

/// Re-reads the settings when the file changed since the last look.
pub fn refresh(now: Instant) {
    let path = path();
    let mut watch = WATCH.lock().unwrap_or_else(PoisonError::into_inner);
    if watch.next_check.is_some_and(|next| now < next) {
        return;
    }
    watch.next_check = Some(now + CHECK_EVERY);
    let current = stamp(&path);
    if current == watch.stamp {
        return;
    }
    watch.stamp = current;
    drop(watch);
    reload(&path);
}

fn reload(path: &PathBuf) {
    let (cfg, warnings) = match fs::read(path) {
        Ok(bytes) => parse(&String::from_utf8_lossy(&bytes)),
        Err(_) => (Config::default(), Vec::new()),
    };
    for warning in &warnings {
        diag::log(&format!("{FILE} {warning}"));
    }
    diag::log(&format!(
        "settings: enabled={} ai={} player={} history={} min_games={} share={} overrides={}",
        cfg.enabled,
        cfg.ai,
        cfg.player,
        cfg.history,
        cfg.min_games,
        cfg.share,
        cfg.overrides.len()
    ));
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(cfg));
}

/// Test support.
#[doc(hidden)]
pub fn set(cfg: Config) {
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(cfg));
}

fn flag(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err(format!("expected on or off, got {value:?}")),
    }
}

pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warnings = Vec::new();
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split([';', '#']).next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("line {}: expected key=value", n + 1));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let result = if section == "positions" {
            let mut lanes = [false; 5];
            let mut bad = Vec::new();
            for part in value.split([',', '/', ' ']).filter(|s| !s.is_empty()) {
                match parse_role(part) {
                    Some(r) => lanes[r] = true,
                    None => bad.push(part),
                }
            }
            if lanes == [false; 5] || !bad.is_empty() {
                Err(format!("expected positions like Mid,Support, got {value:?}"))
            } else {
                cfg.overrides.insert(key.to_lowercase(), lanes);
                Ok(())
            }
        } else {
            set_key(&mut cfg, key, value)
        };
        if let Err(err) = result {
            warnings.push(format!("line {} ({key}): {err}", n + 1));
        }
    }
    if !(0.0..=1.0).contains(&cfg.share) || !cfg.share.is_finite() {
        warnings.push(format!("share={} is out of range, using 0.15", cfg.share));
        cfg.share = 0.15;
    }
    (cfg, warnings)
}

fn set_key(cfg: &mut Config, key: &str, value: &str) -> Result<(), String> {
    match key.to_ascii_lowercase().as_str() {
        "enabled" => cfg.enabled = flag(value)?,
        "ai" => cfg.ai = flag(value)?,
        "player" => cfg.player = flag(value)?,
        "history" => cfg.history = flag(value)?,
        "min_games" => cfg.min_games = value.parse().map_err(|_| format!("expected a whole number, got {value:?}"))?,
        "share" => cfg.share = value.parse().map_err(|_| format!("expected a number, got {value:?}"))?,
        _ => return Err("unknown key (ignored)".to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_gives_the_defaults() {
        let (cfg, warnings) = parse(TEMPLATE);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn values_overrides_and_mistakes() {
        let (cfg, warnings) = parse(
            "[lock]\nai=off\nplayer = no\n[history]\nhistory=off\nshare=2\nmin_games=x\nfoo=1\n[positions]\nAhri = Mid, Support\nzed=Somewhere\n",
        );
        assert!(!cfg.ai && !cfg.player && !cfg.history && cfg.enabled);
        assert_eq!(cfg.share, 0.15, "out of range");
        assert_eq!(cfg.min_games, 8, "not a number");
        assert_eq!(cfg.override_of("AHRI"), Some([false, false, true, false, true]));
        assert_eq!(cfg.override_of("zed"), None);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert_eq!(cfg.rules().min_games, u32::MAX, "history off: main positions only");
    }
}
