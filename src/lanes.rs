//! Which positions a champion can play, which picks still fit a team, and where a team's picks
//! sit.
//!
//! A champion's positions are the first of these that has them:
//! - the player's own list in `settings.ini` (`[positions]`);
//! - the built-in list of League of Legends champions ([`PRESET`], their official main
//!   positions): the game knows nothing about them, so their cards show Top + Jungle;
//! - its two **main positions** as the game shows them on its ban/pick card, learned on the
//!   ban/pick screen and kept in `positions.json` so AI drafts know them too - the positions the
//!   game has seen it played in most (Top + Jungle while it has seen nothing), not something a
//!   champion mod sets; plus every position it has really been played in this save (`history`),
//!   once it has `min_games` games there in all and the position is at least `share` of them.
//!
//! The first position of a list (the player's or the built-in one) is the champion's main
//! position: where it sits when its team has a choice ([`seat`]).
//!
//! A champion with none of these is not restricted. A pick is legal when the team's picks and
//! the candidate can still be seated one per position, each in a position it can play - so two
//! top-only champions are not picked together. When nothing on offer is legal (bans and earlier
//! picks used every fitting champion), everything is: a draft never gets stuck.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

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

/// The champions whose main positions are known (seen on ban/pick cards), by id.
pub fn known_names() -> Vec<String> {
    let guard = MAIN.read().unwrap_or_else(PoisonError::into_inner);
    let mut names: Vec<String> = guard.as_ref().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    names
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

/// The League of Legends champions' official main positions, built in (the same file players
/// were told to copy into `settings.ini` before 1.3.0; their own copy still wins).
pub const PRESET: &str = include_str!("../presets/league_positions.ini");

/// [`PRESET`] read (only its `[positions]` matter).
fn preset() -> &'static Config {
    static PARSED: OnceLock<Config> = OnceLock::new();
    PARSED.get_or_init(|| crate::config::parse(PRESET).0)
}

/// A champion's built-in positions.
pub fn preset_of(champion: &str) -> Option<Lanes> {
    preset().override_of(champion)
}

/// Champions with built-in positions.
pub fn preset_count() -> usize {
    preset().overrides.len()
}

/// Where a champion's positions come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The player's own list (`settings.ini`, `[positions]`).
    Settings,
    /// The built-in list ([`PRESET`]).
    Preset,
    /// The ban/pick card and this save's matches (or nothing: unrestricted).
    Game,
}

pub fn source(champion: &str, cfg: &Config) -> Source {
    if cfg.override_of(champion).is_some() {
        Source::Settings
    } else if preset_of(champion).is_some() {
        Source::Preset
    } else {
        Source::Game
    }
}

/// A champion's main position: the first of its list, the player's or the built-in one.
pub fn main_lane(champion: &str, cfg: &Config) -> Option<usize> {
    match source(champion, cfg) {
        Source::Settings => cfg.main_of(champion),
        Source::Preset => preset().main_of(champion),
        Source::Game => None,
    }
}

/// The positions a champion can play (all of them when nothing is known about it).
pub fn allowed(champion: &str, cfg: &Config, played: Option<&HashMap<String, Played>>) -> Lanes {
    if let Some(lanes) = cfg.override_of(champion) {
        return lanes;
    }
    if let Some(lanes) = preset_of(champion) {
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
    fn place(team: &[Lanes], used: &mut [bool; 5]) -> bool {
        let Some((first, rest)) = team.split_first() else { return true };
        for p in 0..5 {
            if first[p] && !used[p] {
                used[p] = true;
                let ok = place(rest, used);
                used[p] = false;
                if ok {
                    return true;
                }
            }
        }
        false
    }
    place(team, &mut [false; 5])
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

/// The positions a team that picked `team` can still give its next pick: those a champion
/// playing only there would fit in.
pub fn open_positions(team: &[Lanes]) -> Lanes {
    let mut out = [false; 5];
    for (lane, open) in out.iter_mut().enumerate() {
        let mut only = [false; 5];
        only[lane] = true;
        *open = team.len() < 5 && legal(team, only);
    }
    out
}

/// Position names, e.g. `Mid/Support` (`any` for all five).
pub fn describe(lanes: &Lanes) -> String {
    if *lanes == [true; 5] {
        return "any".to_string();
    }
    let names: Vec<&str> = ROLES.iter().enumerate().filter(|(i, _)| lanes[*i]).map(|(_, r)| *r).collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join("/")
    }
}

/// Where a team's picks sit: `order[slot]` is the index (in pick order) of the pick that plays
/// lane slot `slot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seating {
    pub order: Vec<usize>,
    /// Picks seated in a position they can play.
    pub fits: usize,
    /// ... in their main position.
    pub mains: usize,
    /// The players' proficiency with the champions they were given, in all (0 without players).
    pub skill: u64,
    /// Picks left in their own pick slot (the `k`-th pick in the `k`-th slot) and able to play it.
    pub kept: usize,
}

impl Seating {
    fn rank(&self) -> (usize, usize, u64, usize) {
        (self.fits, self.mains, self.skill, self.kept)
    }
}

/// The most picks [`seat`] seats (it tries every order).
pub const MAX_SEATS: usize = 7;

/// The best seating of a team's picks - each one's positions and main position, in pick order -
/// on lane slots (each slot's position index): the most picks in a position they can play, then
/// the most in their main position; of seatings equal in both, the one whose players play their
/// champions best (`skill[slot][pick]`: how well the player of lane slot `slot` plays pick
/// `pick`, the game's champion proficiency - left out when `None` or not one row per slot and
/// one value per pick), then the most picks left in their own pick slot; of equals, the first in
/// order (earlier picks in earlier slots - so of two champions for one position, the earlier pick
/// gets it). Proficiency only ever chooses between seatings that are equal by the first two, so
/// it never seats a champion off its positions. `None` when the counts differ, there are no
/// picks or more than [`MAX_SEATS`], or a slot is not a position.
pub fn seat(picks: &[(Lanes, Option<usize>)], slots: &[usize], skill: Option<&[Vec<u32>]>) -> Option<Seating> {
    let n = picks.len();
    if n == 0 || n != slots.len() || n > MAX_SEATS || slots.iter().any(|s| *s >= 5) {
        return None;
    }
    let skill = skill.filter(|rows| rows.len() == n && rows.iter().all(|r| r.len() == n));
    struct Search<'a> {
        picks: &'a [(Lanes, Option<usize>)],
        slots: &'a [usize],
        skill: Option<&'a [Vec<u32>]>,
        order: Vec<usize>,
        used: [bool; MAX_SEATS],
        best: Option<Seating>,
    }
    fn walk(s: &mut Search<'_>, slot: usize) {
        if slot == s.slots.len() {
            let mut seating = Seating { order: s.order.clone(), fits: 0, mains: 0, skill: 0, kept: 0 };
            for (slot, (pick, lane)) in s.order.iter().zip(s.slots).enumerate() {
                let (lanes, main) = s.picks[*pick];
                seating.fits += usize::from(lanes[*lane]);
                seating.mains += usize::from(main == Some(*lane));
                seating.skill += s.skill.map_or(0, |rows| u64::from(rows[slot][*pick]));
                seating.kept += usize::from(*pick == slot && lanes[*lane]);
            }
            if s.best.as_ref().is_none_or(|b| seating.rank() > b.rank()) {
                s.best = Some(seating);
            }
            return;
        }
        for pick in 0..s.picks.len() {
            if !s.used[pick] {
                s.used[pick] = true;
                s.order.push(pick);
                walk(s, slot + 1);
                s.order.pop();
                s.used[pick] = false;
            }
        }
    }
    let mut search = Search { picks, slots, skill, order: Vec::with_capacity(n), used: [false; MAX_SEATS], best: None };
    walk(&mut search, 0);
    search.best
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

    const MID: Lanes = [false, false, true, false, false];
    const BOT: Lanes = [false, false, false, true, false];
    const SUP: Lanes = [false, false, false, false, true];
    const FIVE: [usize; 5] = [0, 1, 2, 3, 4];

    #[test]
    fn the_open_positions_of_a_team() {
        assert_eq!(open_positions(&[]), [true; 5]);
        assert_eq!(open_positions(&[TOP, JG]), [false, false, true, true, true]);
        // two top/jungle champions hold both, whichever plays where
        assert_eq!(open_positions(&[TOP_JG, TOP_JG]), [false, false, true, true, true]);
        assert_eq!(open_positions(&[TOP, JG, MID, BOT, SUP]), [false; 5], "a full team");
        assert_eq!(describe(&[false, false, true, false, true]), "Mid/Support");
        assert_eq!(describe(&[true; 5]), "any");
    }

    #[test]
    fn a_team_is_seated_where_its_champions_play() {
        // match #4 of the save in the 1.2.0 logs, blue: the game put Garen in the support slot,
        // Seraphine bottom and Varus top - every champion has a position of its own free
        let blue = [(TOP, Some(0)), (SUP_MID, Some(4)), (BOT, Some(3)), (TOP_JG, None), (MID_SUP, Some(2))];
        let s = seat(&blue, &FIVE, None).unwrap();
        assert_eq!(s.order, [0, 3, 4, 2, 1], "top garen, jungle syndra, mid annie, bottom varus, support seraphine");
        assert_eq!((s.fits, s.mains), (5, 4));
        // the same match, red: two junglers (Viego and Briar) - one of them has to go off its
        // position, the other keeps its pick slot; Yasuo plays his main position (mid)
        let red = [(SUP, Some(4)), (JG, Some(1)), (BOT, Some(3)), (MID_TOP, Some(2)), (JG, Some(1))];
        let s = seat(&red, &FIVE, None).unwrap();
        assert_eq!(s.order, [4, 1, 3, 2, 0], "top briar (off), jungle viego, mid yasuo, bottom twitch, support taric");
        assert_eq!((s.fits, s.mains, s.kept), (4, 4, 1));
    }

    #[test]
    fn seating_prefers_main_positions_then_pick_slots() {
        // Yasuo (mid, top) picked first, Kayle (top, mid) third: each plays its main position
        let picks = [(MID_TOP, Some(2)), (JG, None), (TOP_MID, Some(0)), (BOT, None), (SUP, None)];
        assert_eq!(seat(&picks, &FIVE, None).unwrap().order, [2, 1, 0, 3, 4]);
        // nothing to tell two flexible champions apart: they keep their pick slots
        let picks = [(TOP_MID, None), (JG, None), (TOP_MID, None), (BOT, None), (SUP, None)];
        assert_eq!(seat(&picks, &FIVE, None).unwrap().order, FIVE);
        // slots in another order: every champion still goes to its own position
        let picks = [(TOP, None), (JG, None), (MID, None), (BOT, None), (SUP, None)];
        let s = seat(&picks, &[4, 3, 2, 1, 0], None).unwrap();
        assert_eq!((s.order, s.fits), (vec![4, 3, 2, 1, 0], 5));
        // nothing seatable
        assert_eq!(seat(&picks, &FIVE[..4], None), None, "five picks, four slots");
        assert_eq!(seat(&[], &[], None), None);
        assert_eq!(seat(&picks[..1], &[5], None), None, "not a position");
        assert_eq!(seat(&[(TOP, None); 8], &[0; 8], None), None, "too many to try every order");
    }

    /// `skill[slot][pick]` from one row per slot.
    fn skill(rows: &[[u32; 5]]) -> Vec<Vec<u32>> {
        rows.iter().map(|r| r.to_vec()).collect()
    }

    #[test]
    fn of_equally_good_seatings_the_players_proficiency_decides() {
        // Akali and Yasuo both play mid and top (main: mid): one of them goes top either way
        let picks = [(MID_TOP, Some(2)), (JG, None), (MID_TOP, Some(2)), (BOT, None), (SUP, None)];
        assert_eq!(seat(&picks, &FIVE, None).unwrap().order, FIVE, "without players: pick slots");
        // the top laner plays the third pick (Yasuo) far better than the first (Akali)
        let rows = skill(&[[200, 0, 380, 0, 0], [0, 300, 0, 0, 0], [350, 0, 250, 0, 0], [0, 0, 0, 300, 0], [0, 0, 0, 0, 300]]);
        let s = seat(&picks, &FIVE, Some(&rows)).unwrap();
        assert_eq!(s.order, [2, 1, 0, 3, 4], "Yasuo top, Akali mid");
        assert_eq!((s.fits, s.mains, s.skill, s.kept), (5, 1, 380 + 300 + 350 + 300 + 300, 3));
        // proficiency never seats a champion off its positions, nor out of its main position
        let picks = [(TOP, Some(0)), (JG, Some(1)), (MID_TOP, Some(2)), (BOT, None), (SUP, None)];
        let rows = skill(&[[0, 0, 999, 0, 0], [999, 0, 0, 0, 0], [0, 999, 0, 0, 0], [0, 0, 0, 0, 0], [0, 0, 0, 0, 0]]);
        let s = seat(&picks, &FIVE, Some(&rows)).unwrap();
        assert_eq!((s.order, s.fits, s.mains), (FIVE.to_vec(), 5, 3));
        // a matrix that is not one row per slot and one value per pick is left out
        let picks = [(MID_TOP, Some(2)), (JG, None), (MID_TOP, Some(2)), (BOT, None), (SUP, None)];
        let short = skill(&[[0, 0, 999, 0, 0]; 4]);
        assert_eq!(seat(&picks, &FIVE, Some(&short)), seat(&picks, &FIVE, None));
        // nothing known about the players (all zero): exactly the seating without them
        let zero = skill(&[[0; 5]; 5]);
        assert_eq!(seat(&picks, &FIVE, Some(&zero)).unwrap().order, seat(&picks, &FIVE, None).unwrap().order);
    }

    pub const TOP_MID: Lanes = [true, false, true, false, false];
    pub const MID_TOP: Lanes = TOP_MID;
    pub const SUP_MID: Lanes = MID_SUP;

    #[test]
    fn league_champions_use_the_built_in_positions() {
        let _serial = crate::tests::serial();
        clear();
        let cfg = Config::default();
        assert_eq!(preset_count(), 193, "every League champion and the tft pack's Powder, Norra, Alune, Silco, Maddie, Loris, Kobuko, Vander, Violet, Krug, Ao Shin, Scar, Renni, Smeech, Sevika, Steb, Nomsy, Idas, Daeja and Shi Oh Yu");
        // the card said top/jungle (nothing seen yet): the built-in list wins
        learn("league_bard", TOP_JG);
        assert_eq!(allowed("league_bard", &cfg, None), SUP);
        assert_eq!(source("league_bard", &cfg), Source::Preset);
        assert_eq!(allowed("league_viego", &cfg, None), JG);
        assert_eq!(allowed("LEAGUE_AHRI", &cfg, None), MID, "any case");
        // this save's matches do not widen a built-in list (the game used to seat champions
        // anywhere)
        let played: HashMap<String, Played> = [("league_bard".to_string(), [6, 6, 0, 0, 0])].into();
        assert_eq!(allowed("league_bard", &cfg, Some(&played)), SUP);
        // the main position is the list's first
        assert_eq!(main_lane("league_brand", &cfg), Some(4), "Support,Mid");
        assert_eq!(main_lane("league_yasuo", &cfg), Some(2), "Mid,Top");
        // the player's own list wins, its first position too
        let mine = Config {
            overrides: [("league_bard".to_string(), MID_SUP)].into(),
            mains: [("league_bard".to_string(), 2)].into(),
            ..Config::default()
        };
        assert_eq!((allowed("league_bard", &mine, None), main_lane("league_bard", &mine)), (MID_SUP, Some(2)));
        assert_eq!(source("league_bard", &mine), Source::Settings);
        // another mod's champion: the card
        learn("touhou_reimu", SUP);
        assert_eq!((source("touhou_reimu", &cfg), main_lane("touhou_reimu", &cfg)), (Source::Game, None));
        assert_eq!(allowed("touhou_reimu", &cfg, None), SUP);
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
