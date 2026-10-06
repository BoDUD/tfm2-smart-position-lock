//! Which positions a champion can play, and which picks still fit a team.
//!
//! A champion's positions are
//! - its two **main positions** as the game gives them (the position icons on its ban/pick card,
//!   e.g. what a champion mod set for it), learned on the ban/pick screen and kept in
//!   `positions.json` so AI drafts know them too; plus
//! - every position it has really been played in this save (`history`), once it has
//!   `min_games` games there in all and the position is at least `share` of them;
//! - or, instead of both, the player's own list in `settings.ini` (`[positions]`).
//!
//! A champion with none of these is not restricted. A pick is legal when the team's picks and
//! the candidate can still be seated one per position, each in a position it can play - so two
//! top-only champions are not picked together. When nothing on offer is legal (bans and earlier
//! picks used every fitting champion), everything is: a draft never gets stuck.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use crate::config::Config;
use crate::{diag, paths};

/// Positions a champion can play, in [`ROLES`] order.
pub type Lanes = [bool; 5];
/// Games a champion played in each position, in [`ROLES`] order.
pub type Played = [u32; 5];

pub const ROLES: [&str; 5] = ["Top", "Jungle", "Mid", "Bottom", "Support"];
pub const FILE: &str = "positions.json";

/// A position name as the game or a player writes it, as an index into [`ROLES`].
pub fn parse_role(label: &str) -> Option<usize> {
    Some(match label.trim().to_ascii_lowercase().as_str() {
        "top" | "0" => 0,
        "jungle" | "jg" | "jungler" | "1" => 1,
        "mid" | "middle" | "2" => 2,
        "bottom" | "bot" | "adc" | "carry" | "3" => 3,
        "support" | "sup" | "supporter" | "4" => 4,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// Games a champion needs before its own history adds positions.
    pub min_games: u32,
    /// ... and the share of them a position needs.
    pub share: f32,
}

/// The game's main positions per champion, learned from ban/pick cards.
static MAIN: RwLock<Option<HashMap<String, Lanes>>> = RwLock::new(None);
/// Bumped whenever what a champion may play can have changed (for cached answers).
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// This save's games per champion and position (`history`).
static PLAYED: RwLock<Option<Arc<HashMap<String, Played>>>> = RwLock::new(None);

fn read_file(path: &std::path::Path) -> Option<HashMap<String, Lanes>> {
    let map = serde_json::from_slice::<HashMap<String, Vec<String>>>(&std::fs::read(path).ok()?).ok()?;
    Some(
        map.into_iter()
            .map(|(c, lanes)| {
                let mut out = [false; 5];
                for r in lanes.iter().filter_map(|l| parse_role(l)) {
                    out[r] = true;
                }
                (c, out)
            })
            .filter(|(_, lanes)| *lanes != [false; 5])
            .collect(),
    )
}

/// Reads `positions.json` (main positions learned in earlier sessions). The first time, the
/// file Patch Meta AI kept before the lock moved here is taken over.
pub fn load() {
    let dir = paths::mod_dir();
    let mut map = read_file(&dir.join(FILE));
    if map.is_none() {
        let old = dir.parent().map(|p| p.join(crate::compat::PATCH_META_AI).join(FILE));
        if let Some(found) = old.as_deref().and_then(read_file) {
            diag::log(&format!("main positions of {} champions taken over from Patch Meta AI's {FILE}", found.len()));
            map = Some(found);
        }
    }
    let map = map.unwrap_or_default();
    if !map.is_empty() {
        diag::log(&format!("main positions of {} champions from {FILE}", map.len()));
    }
    let take_over = !dir.join(FILE).exists() && !map.is_empty();
    *MAIN.write().unwrap_or_else(PoisonError::into_inner) = Some(map);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if take_over {
        save();
    }
}

/// Writes the learned main positions to `positions.json`.
pub fn save() {
    let guard = MAIN.read().unwrap_or_else(PoisonError::into_inner);
    let Some(map) = guard.as_ref() else { return };
    let out: BTreeMap<&String, Vec<&str>> =
        map.iter().map(|(c, lanes)| (c, ROLES.iter().enumerate().filter(|(i, _)| lanes[*i]).map(|(_, r)| *r).collect())).collect();
    if let Ok(text) = serde_json::to_string_pretty(&out) {
        diag::write_file(FILE, &text);
    }
}

/// Learns a champion's main positions. True when they were new or changed.
pub fn learn(champion: &str, lanes: Lanes) -> bool {
    if lanes == [false; 5] {
        return false;
    }
    let mut guard = MAIN.write().unwrap_or_else(PoisonError::into_inner);
    let changed = guard.get_or_insert_with(HashMap::new).insert(champion.to_string(), lanes) != Some(lanes);
    if changed {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
    changed
}

pub fn main_of(champion: &str) -> Option<Lanes> {
    MAIN.read().unwrap_or_else(PoisonError::into_inner).as_ref()?.get(champion).copied()
}

/// Champions with known main positions.
pub fn known() -> usize {
    MAIN.read().unwrap_or_else(PoisonError::into_inner).as_ref().map_or(0, HashMap::len)
}

/// This save's games per champion and position, as `history` counted them so far.
pub fn set_played(played: Option<Arc<HashMap<String, Played>>>) {
    *PLAYED.write().unwrap_or_else(PoisonError::into_inner) = played;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn played() -> Option<Arc<HashMap<String, Played>>> {
    PLAYED.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Test support.
#[doc(hidden)]
pub fn clear() {
    *MAIN.write().unwrap_or_else(PoisonError::into_inner) = None;
    GENERATION.fetch_add(1, Ordering::Relaxed);
    set_played(None);
}

/// The positions a champion can play (all of them when nothing is known about it).
pub fn allowed(champion: &str, cfg: &Config, played: Option<&HashMap<String, Played>>) -> Lanes {
    if let Some(lanes) = cfg.override_of(champion) {
        return lanes;
    }
    let rules = cfg.rules();
    let mut out = main_of(champion).unwrap_or([false; 5]);
    if let Some(games) = played.and_then(|p| p.get(champion)) {
        let total: u32 = games.iter().sum();
        if total >= rules.min_games {
            for (lane, n) in games.iter().enumerate() {
                if *n >= 2 && *n as f32 / total as f32 >= rules.share {
                    out[lane] = true;
                }
            }
        }
    }
    if out == [false; 5] {
        [true; 5]
    } else {
        out
    }
}

/// Whether the champions can be seated one per position, each in a position it can play.
pub fn fits(team: &[Lanes]) -> bool {
    if team.len() > 5 {
        return false;
    }
    fn seat(team: &[Lanes], used: &mut [bool; 5]) -> bool {
        let Some((first, rest)) = team.split_first() else { return true };
        for p in 0..5 {
            if first[p] && !used[p] {
                used[p] = true;
                let ok = seat(rest, used);
                used[p] = false;
                if ok {
                    return true;
                }
            }
        }
        false
    }
    seat(team, &mut [false; 5])
}

/// Whether a team that picked `team` may pick a champion that plays `cand`.
pub fn legal(team: &[Lanes], cand: Lanes) -> bool {
    if team.len() >= 5 {
        return true;
    }
    let mut all = team.to_vec();
    all.push(cand);
    fits(&all)
}

/// Which of the `open` champions a team that picked `team` may pick: the legal ones, or all of
/// them when none is.
pub fn pickable<'a>(cfg: &Config, team: &[&str], open: &[&'a str]) -> Vec<&'a str> {
    let played = played();
    let played = played.as_deref();
    let lanes: Vec<Lanes> = team.iter().map(|c| allowed(c, cfg, played)).collect();
    let legal: Vec<&str> = open.iter().copied().filter(|c| self::legal(&lanes, allowed(c, cfg, played))).collect();
    if legal.is_empty() {
        open.to_vec()
    } else {
        legal
    }
}

/// Which of the `open` champions an AI team that picked `team` may pick for the player in the
/// position `lane` - its picks fill the players' slots in position order, so its `k`-th pick
/// goes to the `k`-th position. Champions that play that position (and keep the team seatable),
/// else any the team can still seat ([`pickable`]), else all of them.
pub fn pickable_for<'a>(cfg: &Config, team: &[&str], open: &[&'a str], lane: usize) -> Vec<&'a str> {
    let played = played();
    let played = played.as_deref();
    let lanes: Vec<Lanes> = team.iter().map(|c| allowed(c, cfg, played)).collect();
    let fit: Vec<&str> = open
        .iter()
        .copied()
        .filter(|c| {
            let l = allowed(c, cfg, played);
            lane < 5 && l[lane] && self::legal(&lanes, l)
        })
        .collect();
    if fit.is_empty() {
        pickable(cfg, team, open)
    } else {
        fit
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TOP: Lanes = [true, false, false, false, false];
    pub const JG: Lanes = [false, true, false, false, false];
    pub const TOP_JG: Lanes = [true, true, false, false, false];
    pub const MID_SUP: Lanes = [false, false, true, false, true];

    #[test]
    fn seating() {
        assert!(fits(&[TOP, JG]));
        assert!(!fits(&[TOP, TOP]), "two top-only champions");
        assert!(fits(&[TOP, TOP_JG]), "the flexible one takes jungle");
        assert!(!fits(&[TOP, JG, TOP_JG]));
        assert!(legal(&[TOP], MID_SUP) && !legal(&[TOP], TOP));
        assert!(legal(&[TOP, JG, MID_SUP, MID_SUP, [false, false, false, true, false]], TOP), "a full team");
    }

    #[test]
    fn positions_from_the_card_history_and_settings() {
        let _serial = crate::tests::serial();
        clear();
        let cfg = Config::default();
        assert_eq!(allowed("a", &cfg, None), [true; 5], "nothing known: unrestricted");
        assert!(learn("a", MID_SUP));
        assert!(!learn("a", MID_SUP), "nothing new");
        assert_eq!(allowed("a", &cfg, None), MID_SUP);
        // history: 10 games, 3 in the jungle (30%), 1 top (too few)
        let played: HashMap<String, Played> = [("a".to_string(), [1, 3, 6, 0, 0])].into();
        assert_eq!(allowed("a", &cfg, Some(&played)), [false, true, true, false, true]);
        let few = Config { min_games: 20, ..Config::default() };
        assert_eq!(allowed("a", &few, Some(&played)), MID_SUP, "not enough games yet");
        // the player's own list wins
        let mine = Config { overrides: [("a".to_string(), TOP)].into(), ..Config::default() };
        assert_eq!(allowed("A", &mine, Some(&played)), TOP, "any case");
        clear();
    }

    #[test]
    fn pickable_never_leaves_nothing() {
        let _serial = crate::tests::serial();
        clear();
        let cfg = Config::default();
        learn("a", MID_SUP);
        learn("b", MID_SUP);
        learn("c", TOP);
        learn("d", MID_SUP);
        assert_eq!(pickable(&cfg, &["a"], &["b", "c"]), ["b", "c"], "b takes the other of mid/support");
        assert_eq!(pickable(&cfg, &["a", "b"], &["c", "d"]), ["c"], "a and b hold mid and support");
        assert_eq!(pickable(&cfg, &["a", "b"], &["d"]), ["d"], "nothing legal: open up");
        assert_eq!(pickable(&cfg, &["a", "b"], &["d", "unknown"]), ["unknown"], "unknown champions play anywhere");
        clear();
    }

    #[test]
    fn an_ai_pick_fits_the_position_it_goes_to() {
        let _serial = crate::tests::serial();
        clear();
        let cfg = Config::default();
        learn("thresh", [false, false, false, false, true]);
        learn("garen", TOP);
        learn("vi", JG);
        learn("lucian", [false, false, true, true, false]);
        let open = ["thresh", "garen", "vi", "lucian"];
        // the first pick goes to the top laner: only garen
        assert_eq!(pickable_for(&cfg, &[], &open, 0), ["garen"]);
        // the second to the jungler
        assert_eq!(pickable_for(&cfg, &["garen"], &open[..], 1), ["vi"]);
        // the fourth (bottom): lucian
        assert_eq!(pickable_for(&cfg, &["garen", "vi", "x"], &["thresh", "lucian"], 3), ["lucian"]);
        // nobody on offer plays jungle: any the team can still seat
        assert_eq!(pickable_for(&cfg, &["garen"], &["thresh", "lucian"], 1), ["thresh", "lucian"]);
        clear();
    }

    #[test]
    fn positions_file_round_trip_and_take_over() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        let dir = paths::mod_dir();
        let _ = std::fs::remove_file(dir.join(FILE));
        // Patch Meta AI's file next to this mod's folder
        let old_dir = dir.parent().unwrap().join(crate::compat::PATCH_META_AI);
        let _ = std::fs::create_dir_all(&old_dir);
        std::fs::write(old_dir.join(FILE), r#"{"zed":["Mid","Jungle"]}"#).unwrap();
        load();
        assert_eq!(main_of("zed"), Some([false, true, true, false, false]));
        assert!(dir.join(FILE).exists(), "taken over into this mod's folder");
        learn("ahri", MID_SUP);
        save();
        clear();
        load();
        assert_eq!(main_of("ahri"), Some(MID_SUP));
        let _ = std::fs::remove_file(old_dir.join(FILE));
        clear();
    }
}
