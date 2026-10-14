//! The fielded players' proficiency with each champion, so an AI team's champions go to the players
//! who play them best when the lock has a choice (`hook`, `lanes::seat`). Draft hooks cannot read
//! records, so:
//!
//! - the draft hook notes every line-up it is shown ([`fielded`]: `ally_athletes` /
//!   `enemy_athletes`, game 0.6.4+), and `history` every player of this save's competition
//!   matches ([`played`]) - so the athletes of the next match day are known before its drafts;
//! - the client reads those athletes' records ([`Reader`]): `champion_proficiency` (each
//!   champion's `value`), their position ratings (`stat.top` ... `stat.support`) and their name -
//!   on the management screens and the ban/pick screen only, a couple per frame within a small
//!   time budget; fielded players again after [`REFRESH`] (proficiency grows with play);
//! - and publishes them for the hook ([`lineup`]). Another save starts over.
//!
//! A player not read yet is unknown, and a team with an unknown player is seated as if the lock
//! knew none of them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use mod_api_stable::RecordKindV1;
use serde_json::Value;

use crate::diag;
use crate::history::Source;
use crate::lanes::ROLES;

/// Fielded players are read again after this long.
pub const REFRESH: Duration = Duration::from_secs(600);
/// Reading time per frame; one slow record is paid back by the next frames.
const FRAME_BUDGET: Duration = Duration::from_micros(500);
const MAX_READS_PER_FRAME: usize = 2;
const LIST_EVERY: Duration = Duration::from_secs(1);
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// Players kept at most (a league's teams field far fewer).
const MAX_ATHLETES: usize = 600;
/// Tries before an unreadable record is given up for the session.
const MAX_TRIES: u8 = 3;

/// What the lock knows of one player.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Athlete {
    pub name: String,
    /// Champion id -> proficiency (`champion_proficiency.<id>.value`).
    pub proficiency: HashMap<String, u32>,
    /// Rating in each position (`stat.top` ... `stat.support`), when readable.
    pub lanes: Option<[f64; 5]>,
}

impl Athlete {
    /// How well the player plays a champion (0 for one the record does not list).
    pub fn skill(&self, champion: &str) -> u32 {
        self.proficiency.get(champion).copied().unwrap_or(0)
    }

    /// The position the player is rated best in.
    pub fn best_lane(&self) -> Option<usize> {
        let lanes = self.lanes?;
        let best = (0..5).max_by(|a, b| lanes[*a].total_cmp(&lanes[*b]))?;
        (lanes[best] > 0.0).then_some(best)
    }

    /// `Kiin (#598)`, or `#598` without a name.
    pub fn label(&self, id: usize) -> String {
        if self.name.is_empty() {
            format!("#{id}")
        } else {
            format!("{} (#{id})", self.name)
        }
    }
}

/// Why players are wanted: fielded in a draft (read first, kept fresh) or seen in a match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Fielded,
    Played,
}

/// The players read, by athlete id.
type Players = HashMap<usize, Arc<Athlete>>;

static WANTED: Mutex<Option<HashMap<usize, Tier>>> = Mutex::new(None);
static PUBLISHED: RwLock<Option<Arc<Players>>> = RwLock::new(None);

fn want(ids: &[usize], tier: Tier) {
    if ids.is_empty() {
        return;
    }
    let mut guard = WANTED.lock().unwrap_or_else(PoisonError::into_inner);
    let wanted = guard.get_or_insert_with(HashMap::new);
    for id in ids {
        let room = wanted.len() < MAX_ATHLETES || tier == Tier::Fielded;
        match wanted.get_mut(id) {
            Some(t) => *t = (*t).min(tier),
            None if room => {
                wanted.insert(*id, tier);
            }
            None => {}
        }
    }
}

/// Players a draft fielded (the hook: every line-up it is shown).
pub fn fielded(ids: &[usize]) {
    want(ids, Tier::Fielded);
}

/// Players of a match in this save (`history`).
pub fn played(ids: &[usize]) {
    want(ids, Tier::Played);
}

/// What is known of a line-up, in its order; `None` for each player not read yet.
pub fn lineup(ids: &[usize]) -> Vec<Option<Arc<Athlete>>> {
    let published = PUBLISHED.read().unwrap_or_else(PoisonError::into_inner).clone();
    ids.iter().map(|id| published.as_ref().and_then(|p| p.get(id).cloned())).collect()
}

/// Forgets every player, wanted and read (`history`: another save, or the game was left).
pub fn clear() {
    *WANTED.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *PUBLISHED.write().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Test support: these players, as if read.
#[doc(hidden)]
pub fn set(players: HashMap<usize, Athlete>) {
    let map = players.into_iter().map(|(id, a)| (id, Arc::new(a))).collect();
    *PUBLISHED.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(map));
}

/// Reads the wanted players' records, a few per frame (the client).
#[derive(Default)]
pub struct Reader {
    cache: Players,
    read_at: HashMap<usize, Instant>,
    failed: HashMap<usize, u8>,
    todo: VecDeque<usize>,
    next_list: Option<Instant>,
    published_at: Option<Instant>,
    changed: bool,
    debt: Duration,
    reported: HashSet<&'static str>,
}

impl Reader {
    /// Forgets what was read: another save (athlete ids are per save) or the game was left. The
    /// players wanted stay (`history` clears them when the save changes, before it reads the new
    /// save's matches).
    pub fn reset(&mut self) {
        *self = Self::default();
        *PUBLISHED.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// One frame; `read` is false where nothing may be read (in a match, the lock or its AI
    /// switch off).
    pub fn tick(&mut self, src: &mut impl Source, now: Instant, read: bool) {
        if !read {
            return;
        }
        if self.next_list.is_none_or(|t| now >= t) {
            self.next_list = Some(now + LIST_EVERY);
            self.list(now);
        }
        if self.debt > FRAME_BUDGET {
            self.debt -= FRAME_BUDGET;
        } else {
            let started = Instant::now();
            for _ in 0..MAX_READS_PER_FRAME {
                let Some(id) = self.todo.pop_front() else { break };
                self.read(src, id, now);
                if started.elapsed() >= FRAME_BUDGET {
                    break;
                }
            }
            self.debt += started.elapsed().saturating_sub(FRAME_BUDGET);
        }
        if self.changed && (self.todo.is_empty() || self.published_at.is_none_or(|t| now >= t + PUBLISH_EVERY)) {
            self.changed = false;
            self.published_at = Some(now);
            *PUBLISHED.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(self.cache.clone()));
            if self.todo.is_empty() && self.reported.insert("caught-up") {
                diag::log(&format!("[athletes] proficiency of {} players read", self.cache.len()));
            }
        }
    }

    /// The players to read now: fielded ones never read, then played ones never read, then
    /// fielded ones read longer than [`REFRESH`] ago (oldest first).
    fn list(&mut self, now: Instant) {
        let wanted: Vec<(usize, Tier)> = WANTED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|w| w.iter().map(|(id, t)| (*id, *t)).collect())
            .unwrap_or_default();
        let mut todo: Vec<(u8, Option<Instant>, usize)> = wanted
            .into_iter()
            .filter(|(id, _)| self.failed.get(id).is_none_or(|n| *n < MAX_TRIES))
            .filter_map(|(id, tier)| match (self.read_at.get(&id), tier) {
                (None, Tier::Fielded) => Some((0, None, id)),
                (None, Tier::Played) => Some((1, None, id)),
                (Some(at), Tier::Fielded) if now.duration_since(*at) >= REFRESH => Some((2, Some(*at), id)),
                _ => None,
            })
            .collect();
        todo.sort_unstable();
        self.todo = todo.into_iter().map(|(_, _, id)| id).collect();
    }

    fn read(&mut self, src: &mut impl Source, id: usize, now: Instant) {
        match read_athlete(src, id) {
            Ok(athlete) => {
                if self.reported.insert("first") {
                    // what a record gave, once a session
                    let mut best: Vec<(&String, &u32)> = athlete.proficiency.iter().collect();
                    best.sort_unstable_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                    let best: Vec<String> = best.iter().take(3).map(|(c, v)| format!("{c} {v}")).collect();
                    diag::log(&format!(
                        "[athletes] first player read: {}, proficiency with {} champions (best: {}), rated best {}",
                        athlete.label(id),
                        athlete.proficiency.len(),
                        best.join(", "),
                        athlete.best_lane().map_or("- (no position ratings)", |l| ROLES[l])
                    ));
                }
                self.failed.remove(&id);
                self.read_at.insert(id, now);
                self.cache.insert(id, Arc::new(athlete));
                self.changed = true;
            }
            Err(why) => {
                let tries = self.failed.entry(id).or_insert(0);
                *tries += 1;
                if *tries >= MAX_TRIES && self.reported.insert(why) {
                    diag::log(&format!("[athletes] player #{id}: {why} (left out; not logged again for others)"));
                }
            }
        }
    }
}

/// One player's record: name, proficiency, position ratings. The record's own `id` must be the
/// athlete id the draft gave (record ids are athlete ids).
fn read_athlete(src: &mut impl Source, id: usize) -> Result<Athlete, &'static str> {
    let kind = RecordKindV1::Athlete;
    let parse = |json: Option<String>| json.and_then(|j| serde_json::from_str::<Value>(&j).ok());
    let mut own_id = parse(src.record_json(kind, id, "id"));
    let mut name = parse(src.record_json(kind, id, "name"));
    let mut proficiency = parse(src.record_json(kind, id, "champion_proficiency"));
    let mut stat = parse(src.record_json(kind, id, "stat"));
    if proficiency.is_none() {
        // a host that does not read parts of a record: the whole of it
        if let Some(Value::Object(mut doc)) = parse(src.record_json(kind, id, "")) {
            own_id = doc.remove("id");
            name = doc.remove("name");
            proficiency = doc.remove("champion_proficiency");
            stat = doc.remove("stat");
        }
    }
    if let Some(own) = own_id.as_ref().and_then(Value::as_u64) {
        if own != id as u64 {
            return Err("its record holds another athlete id");
        }
    }
    let Some(Value::Object(map)) = proficiency else { return Err("no readable champion proficiency") };
    let proficiency = map
        .into_iter()
        .filter_map(|(champ, v)| {
            let value = v.get("value").and_then(Value::as_f64).or_else(|| v.as_f64())?;
            Some((champ, value.clamp(0.0, u32::MAX as f64) as u32))
        })
        .collect();
    let lanes = stat.as_ref().and_then(|s| {
        let mut out = [0.0; 5];
        for (slot, lane) in out.iter_mut().zip(ROLES) {
            *slot = s.get(lane.to_ascii_lowercase())?.as_f64()?;
        }
        Some(out)
    });
    let name = name.as_ref().and_then(Value::as_str).unwrap_or_default().to_string();
    Ok(Athlete { name, proficiency, lanes })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::history::tests::FakeSave;

    /// An athlete record as the game writes it (the parts the lock reads).
    pub fn record(id: usize, name: &str, best: usize, proficiency: &[(&str, u32)]) -> Value {
        let mut stat = serde_json::Map::new();
        for (i, lane) in ROLES.iter().enumerate() {
            stat.insert(lane.to_ascii_lowercase(), serde_json::json!(if i == best { 90 } else { 0 }));
        }
        stat.insert("aggressive".into(), serde_json::json!(72));
        let prof: serde_json::Map<String, Value> =
            proficiency.iter().map(|(c, v)| (c.to_string(), serde_json::json!({"floor": v / 2, "value": v}))).collect();
        serde_json::json!({"id": id, "name": name, "age": 26, "champion_proficiency": prof, "stat": stat})
    }

    fn run(r: &mut Reader, save: &mut FakeSave, start: Instant, frames: usize) -> Instant {
        let mut now = start;
        for _ in 0..frames {
            r.tick(save, now, true);
            now += Duration::from_millis(16);
        }
        now
    }

    #[test]
    fn reads_the_fielded_players_then_the_played_ones() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        clear();
        let mut r = Reader::default();
        let mut save = FakeSave { team: Some(1), name: "X".into(), ..Default::default() };
        for id in [598, 599, 600, 700] {
            save.records.insert((2, id), record(id, &format!("p{id}"), id % 5, &[("league_garen", 300), ("league_ahri", 120)]));
        }
        // a record that holds another athlete id, and one with nothing readable
        save.records.insert((2, 601), record(42, "other", 0, &[("league_garen", 1)]));
        played(&[700]);
        fielded(&[598, 599, 601, 602]);
        // nothing is read where reading is not allowed
        r.tick(&mut save, Instant::now(), false);
        assert_eq!(save.reads, 0);
        let start = Instant::now();
        // the first frame: fielded players first (at most two a frame)
        r.tick(&mut save, start, true);
        assert!(r.cache.contains_key(&598) && !r.cache.contains_key(&700) && r.cache.len() <= 2);
        // a few listings (one a second): the unreadable ones are tried three times, then left out
        let end = run(&mut r, &mut save, start + Duration::from_millis(16), 250);
        let team = lineup(&[598, 599, 600, 601, 602, 700]);
        let known: Vec<bool> = team.iter().map(Option::is_some).collect();
        assert_eq!(known, [true, true, false, false, false, true], "600 was never wanted, 601/602 unreadable");
        let garen = team[0].as_ref().unwrap();
        assert_eq!((garen.name.as_str(), garen.skill("league_garen"), garen.skill("league_zed")), ("p598", 300, 0));
        assert_eq!(garen.best_lane(), Some(598 % 5));
        assert_eq!(garen.label(598), "p598 (#598)");
        let log = std::fs::read_to_string(crate::paths::mod_dir().join(crate::diag::LOG_FILE)).unwrap();
        assert!(log.contains("its record holds another athlete id"), "{log}");
        assert!(log.contains("[athletes] proficiency of 3 players read"), "{log}");
        // a fielded player is read again after REFRESH, a played one is not
        let reads = save.reads;
        save.records.insert((2, 598), record(598, "p598", 3, &[("league_garen", 350)]));
        run(&mut r, &mut save, end + REFRESH, 10);
        assert_eq!(lineup(&[598])[0].as_ref().unwrap().skill("league_garen"), 350, "read again");
        assert!(save.reads > reads);
        // another save: everything is forgotten
        r.reset();
        assert!(lineup(&[598])[0].is_none());
        clear();
    }

    #[test]
    fn a_host_that_reads_only_whole_records() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        clear();
        let mut r = Reader::default();
        let mut save = FakeSave { team: Some(1), name: "X".into(), whole_only: true, ..Default::default() };
        save.records.insert((2, 5), record(5, "solo", 2, &[("league_ahri", 410)]));
        fielded(&[5]);
        run(&mut r, &mut save, Instant::now(), 5);
        let me = lineup(&[5])[0].clone().expect("read from the whole record");
        assert_eq!((me.skill("league_ahri"), me.best_lane()), (410, Some(2)));
        r.reset();
        clear();
    }
}
