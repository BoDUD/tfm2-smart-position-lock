//! The positions champions really played in the open save: every competition match
//! (`MatchReplay`) and solo-rank game (`SoloRankMatch`), counted per champion and position.
//!
//! Only each record's `blue_team` / `red_team` is read (not the whole replay), a few records
//! per frame on the management screens and within a small time budget, so the game never
//! stutters. Competition players carry their `position`; solo-rank players do not, and get the
//! one-to-one assignment of the side's players to positions with the highest total of their
//! position ratings (`stat.top` ... `stat.support`). Unplayed solo-rank games are looked at
//! again later. Another save (the player's team or its name changes) starts over.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mod_api_stable::RecordKindV1;
use serde_json::{Map, Value};

use crate::diag;
use crate::lanes::{self, parse_role, Played};

/// What is read from the game (the client context, or a test double).
pub trait Source {
    fn record_ids(&mut self, kind: RecordKindV1) -> Vec<usize>;
    fn record_json(&mut self, kind: RecordKindV1, id: usize, path: &str) -> Option<String>;
    fn player_team(&mut self) -> Option<usize>;
    fn team_name(&mut self, team: usize) -> Option<String>;
}

const KINDS: [RecordKindV1; 2] = [RecordKindV1::MatchReplay, RecordKindV1::SoloRankMatch];
const IDENTITY_EVERY: Duration = Duration::from_secs(2);
const LIST_EVERY: Duration = Duration::from_secs(10);
const PUBLISH_EVERY: Duration = Duration::from_secs(1);
/// Reading time per frame; one slow record is paid back by the next frames.
const FRAME_BUDGET: Duration = Duration::from_micros(600);
const MAX_READS_PER_FRAME: usize = 6;
/// Unplayed solo-rank games looked at again per listing (newest first).
const RECHECK_UNPLAYED: usize = 50;

#[derive(Default)]
pub struct History {
    /// The open save: the player's team and its name.
    pub save: Option<(usize, String)>,
    next_identity: Option<Instant>,
    next_list: Option<Instant>,
    queue: VecDeque<(usize, usize)>,
    queued: HashSet<(usize, usize)>,
    /// Record ids read (or found unusable), per kind.
    seen: [HashSet<usize>; 2],
    unplayed: HashSet<usize>,
    counts: HashMap<String, Played>,
    games: [u32; 2],
    changed: bool,
    published_at: Option<Instant>,
    debt: Duration,
    reported: bool,
    /// The competition records listed when the save was opened: any other is a match played
    /// this session, and its positions are checked against the lock (see [`check_match`]).
    at_start: Option<HashSet<usize>>,
}

impl History {
    /// Forgets everything (the game was left).
    pub fn reset(&mut self) {
        *self = Self::default();
        lanes::set_played(None);
    }

    /// One frame: who the save is, what to read, a few records, and publishing the counts.
    /// `read` is false off the management screens (nothing is read there).
    pub fn tick(&mut self, src: &mut impl Source, now: Instant, read: bool) {
        if self.next_identity.is_none_or(|t| now >= t) {
            self.next_identity = Some(now + IDENTITY_EVERY);
            let identity = src.player_team().map(|t| (t, src.team_name(t).unwrap_or_default()));
            if identity != self.save {
                if identity.is_some() {
                    diag::log(&format!("save: team {:?}", identity));
                }
                *self = Self { save: identity, next_identity: self.next_identity, ..Self::default() };
                lanes::set_played(None);
            }
        }
        if self.save.is_none() || !read {
            return;
        }
        if self.next_list.is_none_or(|t| now >= t) {
            self.next_list = Some(now + LIST_EVERY);
            self.list(src);
        }
        if self.debt > FRAME_BUDGET {
            self.debt -= FRAME_BUDGET;
        } else {
            let started = Instant::now();
            for _ in 0..MAX_READS_PER_FRAME {
                let Some((k, id)) = self.queue.pop_front() else { break };
                self.queued.remove(&(k, id));
                self.read(src, k, id);
                if started.elapsed() >= FRAME_BUDGET {
                    break;
                }
            }
            self.debt += started.elapsed().saturating_sub(FRAME_BUDGET);
        }
        if self.changed && self.published_at.is_none_or(|t| now >= t + PUBLISH_EVERY || self.queue.is_empty()) {
            self.changed = false;
            self.published_at = Some(now);
            lanes::set_played(Some(Arc::new(self.counts.clone())));
        }
        if !self.reported && self.queue.is_empty() && self.published_at.is_some() {
            self.reported = true;
            diag::log(&format!(
                "history: {} competition matches, {} solo-rank games, {} champions",
                self.games[0],
                self.games[1],
                self.counts.len()
            ));
        }
    }

    /// Queues the records not read yet, newest first.
    fn list(&mut self, src: &mut impl Source) {
        let mut fresh = Vec::new();
        for (k, kind) in KINDS.iter().enumerate() {
            let mut ids = src.record_ids(*kind);
            ids.sort_unstable_by(|a, b| b.cmp(a));
            if k == 0 && self.at_start.is_none() {
                self.at_start = Some(ids.iter().copied().collect());
            }
            let mut rechecked = 0;
            for id in ids {
                if self.queued.contains(&(k, id)) || self.seen[k].contains(&id) {
                    continue;
                }
                if k == 1 && self.unplayed.contains(&id) {
                    if rechecked >= RECHECK_UNPLAYED {
                        continue;
                    }
                    rechecked += 1;
                }
                fresh.push((k, id));
            }
        }
        self.queued.extend(fresh.iter().copied());
        self.queue.extend(fresh);
    }

    fn read(&mut self, src: &mut impl Source, k: usize, id: usize) {
        let kind = KINDS[k];
        if k == 1 {
            let played = src.record_json(kind, id, "played").and_then(|j| serde_json::from_str::<Value>(&j).ok());
            if played.as_ref().and_then(Value::as_bool) == Some(false) {
                self.unplayed.insert(id);
                return;
            }
        }
        let mut sides: Vec<Value> = ["blue_team", "red_team"]
            .iter()
            .filter_map(|p| src.record_json(kind, id, p))
            .filter_map(|j| serde_json::from_str(&j).ok())
            .collect();
        if sides.len() < 2 {
            // a host that does not read parts of a record: the whole of it
            let doc: Option<Value> = src.record_json(kind, id, "").and_then(|j| serde_json::from_str(&j).ok());
            if let Some(doc) = doc {
                if k == 1 && doc.get("played").and_then(Value::as_bool) == Some(false) {
                    self.unplayed.insert(id);
                    return;
                }
                sides = ["blue_team", "red_team"].iter().filter_map(|p| doc.get(*p).cloned()).collect();
            }
        }
        self.seen[k].insert(id);
        self.unplayed.remove(&id);
        if k == 0 && self.at_start.as_ref().is_some_and(|ids| !ids.contains(&id)) {
            check_match(id, &sides);
        }
        let mut counted = false;
        for side in &sides {
            for (champ, lane) in players(side) {
                if let Some(lane) = lane {
                    self.counts.entry(champ).or_insert([0; 5])[lane] += 1;
                    counted = true;
                }
            }
        }
        if counted {
            self.games[k] += 1;
            self.changed = true;
        } else {
            diag::log_once(&format!("unusable-{k}"), &format!("history: a {kind:?} record without positions (id {id})"));
        }
    }
}

/// A competition match played this session: who played where, after the swap phase - the proof
/// that the lock held to the end. One `[result]` line in `diag.log`.
fn check_match(id: usize, sides: &[Value]) {
    let cfg = crate::config::get();
    if !cfg.enabled {
        return;
    }
    let played = lanes::played();
    let mut wrong = Vec::new();
    let mut total = 0;
    for (side, team) in ["blue", "red"].iter().zip(sides) {
        for (champ, lane) in players(team) {
            let Some(lane) = lane else { continue };
            total += 1;
            if !lanes::allowed(&champ, &cfg, played.as_deref())[lane] {
                wrong.push(format!("{side} {champ} played {}", lanes::ROLES[lane]));
            }
        }
    }
    if total == 0 {
        return;
    }
    diag::log(&if wrong.is_empty() {
        format!("[result] match #{id}: all {total} champions played their positions")
    } else {
        format!("[result] match #{id}: {} of {total} off their positions - {}", wrong.len(), wrong.join(", "))
    });
}

/// The players of one side: (champion, position index). Players are objects with a
/// `champion` (a string or an object with `name`/`id`/`key`) anywhere in the side's data,
/// optionally under `players`/`members`/... - ban lists are skipped.
pub fn players(team: &Value) -> Vec<(String, Option<usize>)> {
    let mut out = Vec::new();
    let mut ratings = Vec::new();
    collect(team, &mut out, &mut ratings, 0);
    if out.len() == 5 && out.iter().all(|(_, l)| l.is_none()) && ratings.iter().all(Option::is_some) {
        let ratings: Vec<[f64; 5]> = ratings.into_iter().flatten().collect();
        for (p, lane) in out.iter_mut().zip(best_assignment(&ratings)) {
            p.1 = Some(lane);
        }
    }
    out
}

fn collect(v: &Value, out: &mut Vec<(String, Option<usize>)>, ratings: &mut Vec<Option<[f64; 5]>>, depth: usize) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Object(map) => {
            if let Some(champ) = champion(map) {
                let lane = ["position", "lane", "role"].iter().find_map(|k| map.get(*k)).and_then(label).and_then(|l| parse_role(&l));
                out.push((champ, lane));
                ratings.push(lane_ratings(map));
                return;
            }
            for (key, child) in map {
                if !key.contains("ban") {
                    collect(child, out, ratings, depth + 1);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect(i, out, ratings, depth + 1)),
        _ => {}
    }
}

fn champion(map: &Map<String, Value>) -> Option<String> {
    match map.get("champion")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(inner) => {
            ["name", "id", "key"].iter().find_map(|k| inner.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty()).map(str::to_string)
        }
        _ => None,
    }
}

/// `"Top"`, a unit enum written as `{"Top": null}`, or an index.
fn label(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(map) if map.len() == 1 => map.keys().next().cloned(),
        _ => None,
    }
}

/// A player's rating in each position (`stat.top` ... `stat.support`).
fn lane_ratings(map: &Map<String, Value>) -> Option<[f64; 5]> {
    let stat = map.get("stat")?.as_object()?;
    let mut out = [0.0; 5];
    for (slot, lane) in out.iter_mut().zip(lanes::ROLES) {
        *slot = stat.get(&lane.to_ascii_lowercase())?.as_f64()?;
    }
    Some(out)
}

/// The position of each of five players with the highest total rating (every permutation).
fn best_assignment(ratings: &[[f64; 5]]) -> [usize; 5] {
    fn permute(k: usize, lanes: &mut [usize; 5], ratings: &[[f64; 5]], best: &mut (f64, [usize; 5])) {
        if k == 1 {
            let total: f64 = lanes.iter().enumerate().map(|(p, l)| ratings[p][*l]).sum();
            if total > best.0 {
                *best = (total, *lanes);
            }
            return;
        }
        for i in 0..k {
            permute(k - 1, lanes, ratings, best);
            if k.is_multiple_of(2) {
                lanes.swap(i, k - 1);
            } else {
                lanes.swap(0, k - 1);
            }
        }
    }
    let mut best = (f64::MIN, [0, 1, 2, 3, 4]);
    permute(5, &mut [0, 1, 2, 3, 4], ratings, &mut best);
    best.1
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A save in memory: records by kind and id, as JSON documents.
    #[derive(Default)]
    pub struct FakeSave {
        pub team: Option<usize>,
        pub name: String,
        pub records: BTreeMap<(usize, usize), Value>,
        /// Paths inside a record cannot be read (only the whole document).
        pub whole_only: bool,
        pub reads: usize,
    }

    fn kind_index(kind: RecordKindV1) -> usize {
        KINDS.iter().position(|k| *k == kind).unwrap_or(9)
    }

    impl Source for FakeSave {
        fn record_ids(&mut self, kind: RecordKindV1) -> Vec<usize> {
            let k = kind_index(kind);
            self.records.keys().filter(|(rk, _)| *rk == k).map(|(_, id)| *id).collect()
        }
        fn record_json(&mut self, kind: RecordKindV1, id: usize, path: &str) -> Option<String> {
            self.reads += 1;
            let doc = self.records.get(&(kind_index(kind), id))?;
            if path.is_empty() {
                return Some(doc.to_string());
            }
            if self.whole_only {
                return None;
            }
            doc.get(path).map(Value::to_string)
        }
        fn player_team(&mut self) -> Option<usize> {
            self.team
        }
        fn team_name(&mut self, _team: usize) -> Option<String> {
            Some(self.name.clone())
        }
    }

    pub fn comp(blue: &[(&str, &str)], red: &[(&str, &str)]) -> Value {
        let side = |s: &[(&str, &str)]| -> Value {
            s.iter().map(|(c, p)| serde_json::json!({"champion": c, "position": p, "athlete_id": 1})).collect()
        };
        serde_json::json!({"version": "1.1", "blue_team_win": true, "blue_team": side(blue), "red_team": side(red),
            "blue_ban": [{"champion": "banned"}], "replay": {"frames": [1, 2, 3]}})
    }

    fn run(h: &mut History, save: &mut FakeSave, frames: usize) {
        let mut now = Instant::now();
        for _ in 0..frames {
            h.tick(save, now, true);
            now += Duration::from_millis(16);
        }
    }

    #[test]
    fn counts_competition_and_solo_positions() {
        let _serial = crate::tests::serial();
        lanes::clear();
        let mut save = FakeSave { team: Some(3), name: "Mods FC".into(), ..Default::default() };
        let five = [("a", "Top"), ("b", "Jungle"), ("c", "Mid"), ("d", "Bottom"), ("e", "Support")];
        for id in 0..4 {
            save.records.insert((0, id), comp(&five, &[("f", "Top"), ("a", "Mid")]));
        }
        // a solo-rank game: no positions, ratings say who plays where
        let rated = |c: &str, best: usize| {
            let mut stat = serde_json::Map::new();
            for (i, r) in lanes::ROLES.iter().enumerate() {
                stat.insert(r.to_lowercase(), serde_json::json!(if i == best { 100 } else { 40 }));
            }
            serde_json::json!({"champion": c, "stat": stat})
        };
        let solo_side: Value = [("a", 2), ("b", 0), ("c", 1), ("d", 4), ("e", 3)].iter().map(|(c, l)| rated(c, *l)).collect();
        save.records.insert((1, 7), serde_json::json!({"played": true, "blue_team": solo_side, "red_team": []}));
        save.records.insert((1, 8), serde_json::json!({"played": false, "blue_team": [], "red_team": []}));
        let mut h = History::default();
        run(&mut h, &mut save, 30);
        let played = lanes::played().expect("published");
        assert_eq!(played["a"], [4, 0, 4 + 1, 0, 0], "top in blue, mid in red, mid in solo");
        assert_eq!(played["e"], [0, 0, 0, 1, 4]);
        assert!(!played.contains_key("banned"), "bans are not players");
        assert_eq!(h.games, [4, 1]);
        assert!(h.unplayed.contains(&8));
        // the unplayed game is played later
        save.records.insert((1, 8), serde_json::json!({"played": true, "blue_team": [{"champion": "z", "position": "Top"}], "red_team": []}));
        h.next_list = None;
        run(&mut h, &mut save, 5);
        assert_eq!(lanes::played().unwrap()["z"], [1, 0, 0, 0, 0]);
        // another save starts over
        save.name = "Other".into();
        h.next_identity = None;
        h.tick(&mut save, Instant::now(), false);
        assert!(lanes::played().is_none() && h.counts.is_empty());
        lanes::clear();
    }

    #[test]
    fn reads_only_the_teams_unless_the_host_cannot() {
        let _serial = crate::tests::serial();
        lanes::clear();
        let five = [("a", "Top"), ("b", "Jungle"), ("c", "Mid"), ("d", "Bottom"), ("e", "Support")];
        for whole_only in [false, true] {
            let mut save = FakeSave { team: Some(1), name: "X".into(), whole_only, ..Default::default() };
            save.records.insert((0, 1), comp(&five, &five));
            let mut h = History::default();
            run(&mut h, &mut save, 5);
            assert_eq!(lanes::played().unwrap()["a"], [2, 0, 0, 0, 0], "whole_only={whole_only}");
        }
        assert_eq!(
            players(&serde_json::json!({"players": [{"champion": {"name": "x"}, "position": {"Jungle": null}}]})),
            [("x".to_string(), Some(1))]
        );
        lanes::clear();
    }

    #[test]
    fn a_match_played_this_session_is_checked() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        crate::config::set(crate::config::Config {
            overrides: [("thresh".to_string(), [false, false, false, false, true])].into(),
            ..crate::config::Config::default()
        });
        let five = [("garen", "Top"), ("vi", "Jungle"), ("ahri", "Mid"), ("jinx", "Bottom"), ("thresh", "Support")];
        let mut save = FakeSave { team: Some(1), name: "X".into(), ..Default::default() };
        save.records.insert((0, 1), comp(&five, &five));
        let mut h = History::default();
        run(&mut h, &mut save, 5);
        // a new match: thresh in the jungle
        let swapped = [("garen", "Top"), ("thresh", "Jungle"), ("ahri", "Mid"), ("jinx", "Bottom"), ("vi", "Support")];
        save.records.insert((0, 2), comp(&five, &swapped));
        h.next_list = None;
        run(&mut h, &mut save, 5);
        let log = std::fs::read_to_string(crate::paths::mod_dir().join(crate::diag::LOG_FILE)).unwrap();
        assert!(!log.contains("match #1"), "a match from before this session is not checked");
        assert!(log.contains("[result] match #2: 1 of 10 off their positions - red thresh played Jungle"), "{log}");
        crate::config::set(crate::config::Config::default());
        lanes::clear();
    }

    #[test]
    fn nothing_is_read_off_the_management_screens() {
        let _serial = crate::tests::serial();
        let mut save = FakeSave { team: Some(1), name: "X".into(), ..Default::default() };
        save.records.insert((0, 1), comp(&[("a", "Top")], &[]));
        let mut h = History::default();
        h.tick(&mut save, Instant::now(), false);
        assert_eq!(save.reads, 0);
        lanes::clear();
    }
}
