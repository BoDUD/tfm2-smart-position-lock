//! The AI teams: what they pick, and where their picks play.
//!
//! - **Picks.** The game scores every candidate itself, then asks the draft hooks; a candidate
//!   the team could not seat one per position together with its earlier picks
//!   (`lanes::pickable`) is scored out of reach, which no other hook's nudge can undo. Bans are
//!   never touched, and nothing is when every candidate on offer is ruled out - the draft never
//!   gets stuck. The game's own choice among the rest (and other mods' nudges to it) is left
//!   alone. The pick order does not matter: the team is seated after its last pick.
//! - **Positions** (`decide_positions`, game 0.6.4+). Once an AI team's picks are in, the game
//!   assigns them to its players' lanes by its own judgement - its players' proficiency and the
//!   positions it has seen the champions played in, whatever the pick order - so a champion
//!   picked as a top laner can end up in support. The lock seats the picks itself
//!   (`lanes::seat`): every champion in a position it can play wherever possible, in its main
//!   position when it has a choice; of seatings equal in both, the one whose players (the
//!   line-up the game hands over, `ally_athletes`, in lane-slot order) play their champions best
//!   by the game's champion proficiency ([`athletes`]) - when every one of them has been read -
//!   else the one that leaves the most picks in their own pick slot. When nothing is known about
//!   any of the picks, the game's assignment stays. Older games never ask: they seat AI teams
//!   themselves.
//!
//! What the game hands over and what the lock did goes to `diag.log`: `[draft]` lines (the first
//! calls in full, then one line per AI pick) and `[seat]` lines (each AI team's picks in pick
//! order and the lanes and players they were given, with the proficiency used, and whether the
//! line-up's players sit in the slots of their best positions); `[result]` lines (`history`) then
//! say where every champion really played.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use mod_api_stable::{StableDraftContext, StableDraftDecision, StableDraftHook};

use crate::athletes::{self, Athlete};
use crate::lanes::{self, Lanes};
use crate::{config, diag};

pub struct LockHook;

/// The score of a pick the lock rules out: below anything the game gives.
pub const LOCKED_SCORE: f32 = -1000.0;
/// Hooks run in priority order (lower first) and the last one to seat a team wins: the lock goes
/// after the usual ones, so its seating and its ruled-out scores are the ones that stand.
pub const PRIORITY: i32 = 10_000;
/// `[draft]` lines per game session at most: while the ban/pick screen is open (the player's own
/// match), and from drafts simulated off screen (the other matches of a match day).
const MAX_LIVE_LINES: u32 = 400;
const MAX_OFF_SCREEN_LINES: u32 = 30;
/// `[seat]` lines for teams seated off screen, per game session.
const MAX_OFF_SCREEN_SEATS: u32 = 60;

static LIVE_LINES: AtomicU32 = AtomicU32::new(0);
static OFF_SCREEN_LINES: AtomicU32 = AtomicU32::new(0);
static OFF_SCREEN_SEATS: AtomicU32 = AtomicU32::new(0);
/// The ban/pick screen is open (set by `screen`): the drafts now are the player's match.
static LIVE: AtomicBool = AtomicBool::new(false);
/// The first hook call of this ban/pick screen was logged.
static LIVE_FIRST: AtomicBool = AtomicBool::new(false);
/// The player's team id (set by the client), `usize::MAX` while unknown.
static PLAYER_TEAM: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The ban/pick screen opened or closed.
pub fn set_live(live: bool) {
    if LIVE.swap(live, Ordering::Relaxed) != live && live {
        LIVE_FIRST.store(false, Ordering::Relaxed);
    }
}

/// The open save's team (to tell the player's team in the log).
pub fn set_player_team(team: Option<usize>) {
    PLAYER_TEAM.store(team.unwrap_or(usize::MAX), Ordering::Relaxed);
}

fn log_capped(tag: &str, msg: &str, off_screen: &AtomicU32, off_screen_max: u32) {
    let live = LIVE.load(Ordering::Relaxed);
    let (count, max) = if live { (&LIVE_LINES, MAX_LIVE_LINES) } else { (off_screen, off_screen_max) };
    let n = count.fetch_add(1, Ordering::Relaxed);
    if n < max {
        let place = if live { "" } else { "(off screen) " };
        let last = if n + 1 == max { " (the last such line this session)" } else { "" };
        diag::log(&format!("[{tag}] {place}{msg}{last}"));
    }
}

fn log_draft(msg: &str) {
    log_capped("draft", msg, &OFF_SCREEN_LINES, MAX_OFF_SCREEN_LINES);
}

fn log_seat(msg: &str) {
    log_capped("seat", msg, &OFF_SCREEN_SEATS, MAX_OFF_SCREEN_SEATS);
}

/// The game's champion table (draft ids index into it), kept from calls that came with it: some
/// calls give the ids without the table.
static TABLE: RwLock<Vec<String>> = RwLock::new(Vec::new());
static NAMELESS_LOGGED: AtomicU32 = AtomicU32::new(0);

fn remember_table(ctx: &StableDraftContext<'_>) {
    let briefs = ctx.champion_briefs().len();
    if briefs == 0 {
        return;
    }
    {
        // the same table: same length, same first and last names
        let table = TABLE.read().unwrap_or_else(PoisonError::into_inner);
        let same = |id: usize| table.get(id).map(String::as_str) == ctx.champion_name(id);
        if table.len() == briefs && same(0) && same(briefs - 1) {
            return;
        }
    }
    let names: Vec<String> = (0..briefs).map(|id| ctx.champion_name(id).unwrap_or_default().to_string()).collect();
    *TABLE.write().unwrap_or_else(PoisonError::into_inner) = names;
}

fn name_of(ctx: &StableDraftContext<'_>, id: usize) -> Option<String> {
    ctx.champion_name(id)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| TABLE.read().unwrap_or_else(PoisonError::into_inner).get(id).filter(|n| !n.is_empty()).cloned())
}

/// One pick decision as the game describes it: the team's picks and the candidates, by name.
struct View {
    ally: Vec<String>,
    offer: Vec<(usize, String)>,
}

impl View {
    fn read(ctx: &StableDraftContext<'_>) -> Option<Self> {
        remember_table(ctx);
        let ally: Option<Vec<String>> = ctx.ally_picks().iter().map(|id| name_of(ctx, *id)).collect();
        let offer: Option<Vec<(usize, String)>> =
            ctx.available_champions().iter().map(|id| Some((*id, name_of(ctx, *id)?))).collect();
        match (ally, offer) {
            (Some(ally), Some(offer)) => Some(Self { ally, offer }),
            _ => {
                if NAMELESS_LOGGED.fetch_add(1, Ordering::Relaxed) < 5 {
                    let missing: Vec<usize> = ctx
                        .ally_picks()
                        .iter()
                        .chain(ctx.available_champions())
                        .copied()
                        .filter(|id| name_of(ctx, *id).is_none())
                        .take(6)
                        .collect();
                    diag::log(&format!(
                        "[draft] champion ids without names - this pick is not judged: ids {missing:?}, \
                         {} names given, {} kept from earlier calls, {} on offer, team picks {}",
                        ctx.champion_briefs().len(),
                        TABLE.read().unwrap_or_else(PoisonError::into_inner).len(),
                        ctx.available_champions().len(),
                        ctx.ally_picks().len()
                    ));
                }
                None
            }
        }
    }

    fn ally(&self) -> Vec<&str> {
        self.ally.iter().map(String::as_str).collect()
    }

    fn names(&self) -> Vec<&str> {
        self.offer.iter().map(|(_, n)| n.as_str()).collect()
    }
}

/// What makes one pick decision: the team's and the enemy's picks, the offer (ids - cheap to
/// compare on every call), whether it is a look-ahead, and the rules.
fn decision_key(ctx: &StableDraftContext<'_>, cfg: &config::Config) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (ctx.ally_picks(), ctx.enemy_picks(), ctx.available_champions(), ctx.is_explore()).hash(&mut h);
    (lanes::generation(), cfg as *const config::Config as usize).hash(&mut h);
    h.finish()
}

/// The decision in progress on this thread: the game scores every candidate of one decision in a
/// row (hooks may run on several threads).
#[derive(Default)]
struct Decision {
    key: Option<u64>,
    /// The ids the team may pick; `None` when the champions could not be named.
    ok: Option<HashSet<usize>>,
}

impl Decision {
    fn build(ctx: &StableDraftContext<'_>, cfg: &config::Config, key: u64) -> Self {
        // the line-ups of this draft: their proficiency is read before the teams are seated
        athletes::fielded(ctx.ally_athletes());
        athletes::fielded(ctx.enemy_athletes());
        let Some(view) = View::read(ctx) else { return Self { key: Some(key), ok: None } };
        let offer = view.names();
        let ally = view.ally();
        let ok: Vec<&str> = if ally.len() >= 5 { offer.clone() } else { lanes::pickable(cfg, &ally, &offer) };
        log_decision(cfg, &view, &ok);
        let ids = view.offer.iter().filter(|(_, n)| ok.contains(&n.as_str())).map(|(id, _)| *id).collect();
        Self { key: Some(key), ok: Some(ids) }
    }
}

thread_local! {
    static CURRENT: std::cell::RefCell<Decision> = std::cell::RefCell::new(Decision::default());
}

impl StableDraftHook for LockHook {
    fn id(&self) -> String {
        format!("{}:lock", crate::MOD_ID)
    }

    fn priority(&self) -> i32 {
        PRIORITY
    }

    fn score_ban(&self, ctx: &StableDraftContext<'_>, _candidate: usize, _base: f32) -> StableDraftDecision {
        first_call(ctx, &FIRST_BAN, "score_ban");
        StableDraftDecision::Pass
    }

    fn score_pick(&self, ctx: &StableDraftContext<'_>, candidate: usize, _base: f32) -> StableDraftDecision {
        first_call(ctx, &FIRST_PICK, "score_pick");
        let cfg = config::get();
        if !cfg.enabled || !cfg.ai {
            return StableDraftDecision::Pass;
        }
        let key = decision_key(ctx, &cfg);
        CURRENT.with(|cell| {
            let mut d = cell.borrow_mut();
            if d.key != Some(key) {
                *d = Decision::build(ctx, &cfg, key);
            }
            match &d.ok {
                // a candidate not on offer (should not happen) is left to the game
                Some(ok) if ctx.available_champions().contains(&candidate) && !ok.contains(&candidate) => {
                    StableDraftDecision::Replace(LOCKED_SCORE)
                }
                _ => StableDraftDecision::Pass,
            }
        })
    }

    fn decide_positions(&self, ctx: &StableDraftContext<'_>) -> Option<Vec<usize>> {
        first_call(ctx, &FIRST_SEAT, "decide_positions");
        let team = team_label(ctx.ally_team());
        let cfg = config::get();
        if !cfg.enabled || !cfg.ai {
            diag::log_once("seat-off", "[seat] the lock or its AI switch is off: the game seats AI teams itself");
            return None;
        }
        athletes::fielded(ctx.ally_athletes());
        athletes::fielded(ctx.enemy_athletes());
        remember_table(ctx);
        let Some(names) = ctx.ally_picks().iter().map(|id| name_of(ctx, *id)).collect::<Option<Vec<String>>>() else {
            log_seat(&format!("{team}: champion ids without names {:?} - the game seats it itself", ctx.ally_picks()));
            return None;
        };
        let picks: Vec<&str> = names.iter().map(String::as_str).collect();
        let slots: Vec<usize> = ctx.slot_lanes().iter().map(|l| l.code() as usize).collect();
        let lineup = Lineup::read(ctx.ally_athletes(), &picks, slots.len());
        let plan = plan_seats(&cfg, &picks, &slots, lineup.skill.as_deref());
        if !ctx.is_explore() {
            log_seat(&format!("{team}: {}", describe_plan(&cfg, &plan, &picks, &slots, &lineup)));
            check_lineup_order(&team, &lineup, &slots);
        }
        match plan {
            Plan::Seat(seating) => Some(seating.order),
            Plan::Keep(_) => None,
        }
    }
}

/// The players an AI team fields, in lane-slot order, as far as the lock knows them.
pub struct Lineup {
    /// Athlete ids as the game gave them (empty: no line-up, e.g. an older game).
    pub ids: Vec<usize>,
    /// What is known of each (`None`: not read yet).
    pub players: Vec<Option<Arc<Athlete>>>,
    /// `skill[slot][pick]`: the proficiency of each slot's player with each pick, when every
    /// player is known.
    pub skill: Option<Vec<Vec<u32>>>,
}

impl Lineup {
    /// The line-up `ids` for a team that picked `picks` and fills `slots` lane slots.
    pub fn read(ids: &[usize], picks: &[&str], slots: usize) -> Self {
        let players = athletes::lineup(ids);
        let complete = !players.is_empty() && players.len() == slots && players.iter().all(Option::is_some);
        let skill = complete.then(|| players.iter().flatten().map(|p| picks.iter().map(|c| p.skill(c)).collect()).collect());
        Self { ids: ids.to_vec(), players, skill }
    }

    fn label(&self, slot: usize) -> Option<String> {
        let id = *self.ids.get(slot)?;
        Some(match self.players.get(slot).and_then(Option::as_ref) {
            Some(p) => p.label(id),
            None => format!("#{id}"),
        })
    }
}

/// "team #63", with "(yours)" for the player's.
fn team_label(team: Option<usize>) -> String {
    match team {
        Some(id) if id == PLAYER_TEAM.load(Ordering::Relaxed) => format!("team #{id} (yours)"),
        Some(id) => format!("team #{id}"),
        None => "a team".to_string(),
    }
}

/// What the lock does with an AI team's picks once they are in.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// The game's own seating stays, and why.
    Keep(&'static str),
    /// The lock's seating.
    Seat(lanes::Seating),
}

/// How a team that picked `picks` (champion ids in pick order) sits on the lane slots `slots`
/// (each slot's position index): the lock's seating when it knows where any of them may play;
/// `skill` (the slots' players' proficiency with each pick) chooses between equally good ones.
pub fn plan_seats(cfg: &config::Config, picks: &[&str], slots: &[usize], skill: Option<&[Vec<u32>]>) -> Plan {
    let mut five = slots.to_vec();
    five.sort_unstable();
    if five != [0, 1, 2, 3, 4] {
        return Plan::Keep("the lane slots are not the five positions");
    }
    if picks.len() != slots.len() {
        return Plan::Keep("not one pick per lane slot");
    }
    let played = lanes::played();
    let held: Vec<(Lanes, Option<usize>)> =
        picks.iter().map(|c| (lanes::allowed(c, cfg, played.as_deref()), lanes::main_lane(c, cfg))).collect();
    if held.iter().all(|(l, _)| *l == [true; 5]) {
        return Plan::Keep("nothing is known about where these champions play");
    }
    match lanes::seat(&held, slots, skill) {
        Some(seating) => Plan::Seat(seating),
        None => Plan::Keep("no seating found"),
    }
}

/// One `[seat]` line: the picks in pick order with their positions, then the lane and player
/// each got (with the player's proficiency with it), and what the proficiency did.
fn describe_plan(cfg: &config::Config, plan: &Plan, picks: &[&str], slots: &[usize], lineup: &Lineup) -> String {
    let played = lanes::played();
    let held: Vec<Lanes> = picks.iter().map(|c| lanes::allowed(c, cfg, played.as_deref())).collect();
    let picked: Vec<String> = picks.iter().zip(&held).map(|(c, l)| format!("{c} ({})", lanes::describe(l))).collect();
    let picked = if picked.is_empty() { "none".to_string() } else { picked.join(", ") };
    let s = match plan {
        Plan::Keep(why) => return format!("picks in pick order: {picked} - {why}: the game seats them itself"),
        Plan::Seat(s) => s,
    };
    let seated: Vec<String> = s
        .order
        .iter()
        .zip(slots)
        .enumerate()
        .map(|(slot, (pick, lane))| {
            let off = if held[*pick][*lane] { "" } else { " (off its positions)" };
            let player = match (lineup.label(slot), lineup.players.get(slot).and_then(Option::as_ref)) {
                (Some(label), Some(p)) => format!(" [{label}: {}]", p.skill(picks[*pick])),
                (Some(label), None) => format!(" [{label}]"),
                _ => String::new(),
            };
            format!("{} {}{off}{player}", lanes::ROLES[*lane], picks[*pick])
        })
        .collect();
    let all = if s.fits == picks.len() { "all".to_string() } else { format!("{} of", s.fits) };
    let skill = match &lineup.skill {
        _ if lineup.ids.is_empty() => "no line-up from the game: players' proficiency not used".to_string(),
        None if lineup.ids.len() != slots.len() => {
            format!("players' proficiency not used: a line-up of {} players for {} lane slots", lineup.ids.len(), slots.len())
        }
        None => {
            let unknown = lineup.players.iter().filter(|p| p.is_none()).count();
            format!("players' proficiency not used: {unknown} of {} players not read yet", lineup.players.len())
        }
        Some(rows) => {
            let total = |order: &[usize]| -> u64 { order.iter().enumerate().map(|(slot, pick)| u64::from(rows[slot][*pick])).sum() };
            match plan_seats(cfg, picks, slots, None) {
                Plan::Seat(plain) if plain.order != s.order => format!(
                    "players' proficiency {} chose this seating over an equally good one ({})",
                    s.skill,
                    total(&plain.order)
                ),
                _ => format!("players' proficiency {} (the same seating without it)", s.skill),
            }
        }
    };
    format!("picks in pick order: {picked} -> seated: {}; {all} {} in their positions; {skill}", seated.join(", "), picks.len())
}

/// Line-ups whose order was checked against the players' best positions (logged), and the ones
/// that did not match (logged too, up to a limit).
static ORDER_CHECKED: AtomicU32 = AtomicU32::new(0);
static ORDER_MISMATCHED: AtomicU32 = AtomicU32::new(0);
const MAX_ORDER_CHECKS: u32 = 3;
const MAX_ORDER_MISMATCHES: u32 = 10;

/// Whether the line-up the game handed over is in lane-slot order: each player sits in the slot
/// of the position he is rated best in (a substitute out of his position would not). Logged for
/// the first line-ups whose ratings are all known, and for every one that does not match.
fn check_lineup_order(team: &str, lineup: &Lineup, slots: &[usize]) {
    if lineup.ids.is_empty() || lineup.ids.len() != slots.len() {
        if !lineup.ids.is_empty() {
            diag::log_once("lineup-size", &format!("[seat] {team}: a line-up of {} players for {} lane slots", lineup.ids.len(), slots.len()));
        }
        return;
    }
    let best: Option<Vec<usize>> = lineup.players.iter().map(|p| p.as_ref()?.best_lane()).collect();
    let Some(best) = best else { return };
    let matched = best.iter().zip(slots).all(|(b, s)| b == s);
    let (count, max) = if matched { (&ORDER_CHECKED, MAX_ORDER_CHECKS) } else { (&ORDER_MISMATCHED, MAX_ORDER_MISMATCHES) };
    if count.fetch_add(1, Ordering::Relaxed) >= max {
        return;
    }
    let seats: Vec<String> = slots
        .iter()
        .zip(&best)
        .enumerate()
        .map(|(slot, (lane, b))| {
            let label = lineup.label(slot).unwrap_or_default();
            if b == lane {
                format!("{} {label}", lanes::ROLES[*lane])
            } else {
                format!("{} {label} (rated best {})", lanes::ROLES[*lane], lanes::ROLES[*b])
            }
        })
        .collect();
    diag::log(&format!(
        "[seat] {team}'s line-up by lane slot: {} - {}",
        seats.join(", "),
        if matched {
            "every player sits in the slot of his best position: the line-up is in lane-slot order"
        } else {
            "NOT every player sits in the slot of his best position (another order, or a player out of his position)"
        }
    ));
}

static FIRST_BAN: AtomicBool = AtomicBool::new(false);
static FIRST_PICK: AtomicBool = AtomicBool::new(false);
static FIRST_SEAT: AtomicBool = AtomicBool::new(false);

/// The first hook call of the session, in full: what the game hands over.
fn first_call(ctx: &StableDraftContext<'_>, first: &AtomicBool, what: &str) {
    if LIVE.load(Ordering::Relaxed) && !LIVE_FIRST.swap(true, Ordering::Relaxed) {
        log_draft(&format!(
            "first call on this ban/pick screen ({what}): {} champion names, {} on offer, team picks {}, enemy picks {}",
            ctx.champion_briefs().len(),
            ctx.available_champions().len(),
            ctx.ally_picks().len(),
            ctx.enemy_picks().len()
        ));
    }
    if first.swap(true, Ordering::Relaxed) {
        return;
    }
    diag::log(&{
        let names = ctx.champion_briefs().len();
        let sample: Vec<&str> = ctx.available_champions().iter().take(3).filter_map(|id| ctx.champion_name(*id)).collect();
        let slots: Vec<&str> = ctx.slot_lanes().iter().filter_map(|l| lanes::ROLES.get(l.code() as usize).copied()).collect();
        let team = |t: Option<usize>| t.map_or("?".to_string(), |t| t.to_string());
        format!(
            "[draft] first {what}: phase {:?}, explore {}, {} champion names (e.g. {}), {} on offer, team picks {}, \
             enemy picks {}, lane slots [{}], players {} / {}, teams {} vs {}",
            ctx.phase(),
            ctx.is_explore(),
            names,
            sample.join(", "),
            ctx.available_champions().len(),
            ctx.ally_picks().len(),
            ctx.enemy_picks().len(),
            slots.join(", "),
            ctx.ally_athletes().len(),
            ctx.enemy_athletes().len(),
            team(ctx.ally_team()),
            team(ctx.enemy_team())
        )
    });
}

fn log_decision(cfg: &config::Config, view: &View, ok: &[&str]) {
    let out: Vec<&str> = view.names().into_iter().filter(|n| !ok.contains(n)).collect();
    let team = if view.ally.is_empty() { "nobody yet".to_string() } else { view.ally.join(", ") };
    let ruled = if out.is_empty() {
        "nothing ruled out".to_string()
    } else {
        let shown: Vec<&str> = out.iter().take(8).copied().collect();
        format!("ruled out {}: {}{}", out.len(), shown.join(", "), if out.len() > 8 { ", ..." } else { "" })
    };
    let played = lanes::played();
    let held: Vec<Lanes> = view.ally.iter().map(|c| lanes::allowed(c, cfg, played.as_deref())).collect();
    log_draft(&format!(
        "AI pick {}, team so far: {team} (open: {}); {} on offer, {} fit, {ruled}",
        view.ally.len() + 1,
        lanes::describe(&lanes::open_positions(&held)),
        view.offer.len(),
        ok.len()
    ));
}

/// Whether a team that picked `ally` may pick `cand` out of `available` (tests and tools).
pub fn allows(cfg: &config::Config, cand: &str, ally: &[&str], available: &[&str]) -> bool {
    if ally.len() >= 5 || !available.contains(&cand) {
        return true;
    }
    lanes::pickable(cfg, ally, available).contains(&cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::tests::{MID_SUP, TOP};
    use mod_api_stable::{ChampionBriefV1, DraftCtxV1, DraftPhaseV1, LaneV1, StrV1};
    use std::collections::HashMap;

    #[test]
    fn rules_out_picks_no_open_position_can_take() {
        let _serial = crate::tests::serial();
        lanes::clear();
        let cfg = config::Config::default();
        lanes::learn("a", TOP);
        lanes::learn("b", TOP);
        lanes::learn("c", MID_SUP);
        assert!(!allows(&cfg, "b", &["a"], &["b", "c"]), "a holds top");
        assert!(allows(&cfg, "c", &["a"], &["b", "c"]));
        assert!(allows(&cfg, "b", &["a"], &["b"]), "nothing else on offer: the draft goes on");
        assert!(allows(&cfg, "b", &["a", "c", "x", "y", "z"], &["b"]), "a full team");
        lanes::clear();
    }

    /// A draft as the game hands it to a hook: the champion table, the team's picks, the offer
    /// and (in the position phase) the lane slots. `size` cuts the context to an older host's.
    struct FakeDraft {
        _names: Vec<String>,
        _briefs: Vec<ChampionBriefV1>,
        _ally: Vec<usize>,
        _available: Vec<usize>,
        _slots: Vec<u32>,
        athletes: Vec<usize>,
        raw: DraftCtxV1,
    }

    impl FakeDraft {
        fn new(phase: DraftPhaseV1, table: &[&str], ally: &[&str], available: &[&str], slots: &[LaneV1]) -> Self {
            let id = |c: &&str| table.iter().position(|t| t == c).expect("in the table");
            let names: Vec<String> = table.iter().map(|s| s.to_string()).collect();
            let briefs: Vec<ChampionBriefV1> =
                names.iter().map(|n| ChampionBriefV1 { name: StrV1::from_str(n), ..ChampionBriefV1::default() }).collect();
            let ally: Vec<usize> = ally.iter().map(id).collect();
            let available: Vec<usize> = available.iter().map(id).collect();
            let slots: Vec<u32> = slots.iter().map(|l| l.code()).collect();
            let raw = DraftCtxV1 {
                size: std::mem::size_of::<DraftCtxV1>(),
                phase: phase.code(),
                difficulty: 1,
                is_explore: false,
                available_ptr: available.as_ptr(),
                available_len: available.len(),
                ally_ban_ptr: std::ptr::null(),
                ally_ban_len: 0,
                enemy_ban_ptr: std::ptr::null(),
                enemy_ban_len: 0,
                ally_pick_ptr: ally.as_ptr(),
                ally_pick_len: ally.len(),
                enemy_pick_ptr: std::ptr::null(),
                enemy_pick_len: 0,
                champion_briefs_ptr: briefs.as_ptr(),
                champion_briefs_len: briefs.len(),
                ally_athletes_ptr: std::ptr::null(),
                ally_athletes_len: 0,
                enemy_athletes_ptr: std::ptr::null(),
                enemy_athletes_len: 0,
                ally_team: 63,
                enemy_team: 64,
                slot_lanes_ptr: slots.as_ptr(),
                slot_lanes_len: slots.len(),
            };
            Self { _names: names, _briefs: briefs, _ally: ally, _available: available, _slots: slots, athletes: Vec::new(), raw }
        }

        /// The team's line-up (athlete ids in lane-slot order).
        fn with_lineup(mut self, ids: &[usize]) -> Self {
            self.athletes = ids.to_vec();
            self.raw.ally_athletes_ptr = self.athletes.as_ptr();
            self.raw.ally_athletes_len = self.athletes.len();
            self
        }

        fn ctx(&self) -> StableDraftContext<'_> {
            // SAFETY: `raw` points into vectors this value owns, alive while it is borrowed.
            unsafe { StableDraftContext::from_raw(&self.raw) }.expect("a valid context")
        }
    }

    const LANES: [LaneV1; 5] = [LaneV1::Top, LaneV1::Jungle, LaneV1::Mid, LaneV1::Bottom, LaneV1::Support];

    fn log() -> String {
        std::fs::read_to_string(crate::paths::mod_dir().join(crate::diag::LOG_FILE)).unwrap_or_default()
    }

    #[test]
    fn an_ai_team_is_seated_where_its_champions_play() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        config::set(config::Config::default());
        set_live(false);
        set_player_team(None);
        FIRST_SEAT.store(false, Ordering::Relaxed);
        // League champions (built-in positions), picked in an order that is no lane order
        let table = ["league_seraphine", "league_garen", "league_annie", "league_viego", "league_varus", "league_briar"];
        let picks = &table[..5];
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, picks, &[], &LANES);
        let order = LockHook.decide_positions(&draft.ctx());
        // top garen, jungle viego, mid annie, bottom varus, support seraphine
        assert_eq!(order, Some(vec![1, 3, 2, 4, 0]));
        let text = log();
        assert!(
            text.contains(
                "[seat] (off screen) team #63: picks in pick order: league_seraphine (Mid/Support), league_garen (Top), \
                 league_annie (Mid/Support), league_viego (Jungle), league_varus (Bottom) -> seated: Top league_garen, \
                 Jungle league_viego, Mid league_annie, Bottom league_varus, Support league_seraphine; all 5 in their \
                 positions"
            ),
            "{text}"
        );
        assert!(
            text.contains("[draft] first decide_positions: phase Some(Position)")
                && text.contains("lane slots [Top, Jungle, Mid, Bottom, Support], players 0 / 0, teams 63 vs 64"),
            "{text}"
        );

        // two junglers: one plays off its positions, the other keeps the jungle
        let picks = ["league_seraphine", "league_garen", "league_annie", "league_viego", "league_briar"];
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &picks, &[], &LANES);
        set_player_team(Some(63));
        assert_eq!(LockHook.decide_positions(&draft.ctx()), Some(vec![1, 3, 2, 4, 0]));
        assert!(log().contains("team #63 (yours): picks in pick order"), "the player's team is named");
        assert!(log().contains("Bottom league_briar (off its positions), Support league_seraphine; 4 of 5 in their positions"));
        set_player_team(None);
        lanes::clear();
    }

    /// A player rated `best` in one position, with these proficiencies.
    fn player(name: &str, best: usize, proficiency: &[(&str, u32)]) -> Athlete {
        let mut lanes = [0.0; 5];
        lanes[best] = 90.0;
        Athlete {
            name: name.to_string(),
            proficiency: proficiency.iter().map(|(c, v)| (c.to_string(), *v)).collect(),
            lanes: Some(lanes),
        }
    }

    #[test]
    fn of_equally_good_seatings_each_champion_goes_to_the_player_who_plays_it_best() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        athletes::clear();
        config::set(config::Config::default());
        set_live(false);
        set_player_team(None);
        ORDER_CHECKED.store(0, Ordering::Relaxed);
        ORDER_MISMATCHED.store(0, Ordering::Relaxed);
        // Akali and Yasuo both play mid and top (main: mid): one of them goes top either way
        let table = ["league_akali", "league_viego", "league_yasuo", "league_varus", "league_bard"];
        let lineup = [598, 599, 600, 601, 602];
        let seat = |ids: &[usize]| {
            let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &table, &[], &LANES).with_lineup(ids);
            LockHook.decide_positions(&draft.ctx())
        };
        // nobody read yet: exactly as without players (pick slots: Akali top, Yasuo mid)
        assert_eq!(seat(&lineup), Some(vec![0, 1, 2, 3, 4]));
        assert!(log().contains("players' proficiency not used: 5 of 5 players not read yet"), "{}", log());
        assert!(athletes::lineup(&lineup).iter().all(Option::is_none));
        // the top laner plays Yasuo far better than Akali, the mid laner the other way round
        let players = HashMap::from([
            (598, player("Kiin", 0, &[("league_akali", 200), ("league_yasuo", 380)])),
            (599, player("Canyon", 1, &[("league_viego", 300)])),
            (600, player("Chovy", 2, &[("league_akali", 350), ("league_yasuo", 250)])),
            (601, player("Ruler", 3, &[("league_varus", 300)])),
            (602, player("Lehends", 4, &[("league_bard", 300)])),
        ]);
        athletes::set(players.clone());
        assert_eq!(seat(&lineup), Some(vec![2, 1, 0, 3, 4]), "Yasuo top, Akali mid");
        let text = log();
        assert!(
            text.contains(
                "-> seated: Top league_yasuo [Kiin (#598): 380], Jungle league_viego [Canyon (#599): 300], \
                 Mid league_akali [Chovy (#600): 350], Bottom league_varus [Ruler (#601): 300], Support league_bard \
                 [Lehends (#602): 300]; all 5 in their positions; players' proficiency 1630 chose this seating over \
                 an equally good one (1350)"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "[seat] team #63's line-up by lane slot: Top Kiin (#598), Jungle Canyon (#599), Mid Chovy (#600), \
                 Bottom Ruler (#601), Support Lehends (#602) - every player sits in the slot of his best position"
            ),
            "{text}"
        );
        // one player not read: exactly as without players
        let mut four = players.clone();
        four.remove(&600);
        athletes::set(four);
        assert_eq!(seat(&lineup), Some(vec![0, 1, 2, 3, 4]));
        assert!(log().contains("players' proficiency not used: 1 of 5 players not read yet"));
        // no line-up (an older game, or the game does not know it): exactly as without players
        athletes::set(players);
        assert_eq!(seat(&[]), Some(vec![0, 1, 2, 3, 4]));
        assert!(log().contains("no line-up from the game: players' proficiency not used"));
        // a line-up in another order is seen in the log (and seated by it: Chovy, the mid laner,
        // still plays Akali better)
        assert_eq!(seat(&[599, 598, 600, 601, 602]), Some(vec![2, 1, 0, 3, 4]));
        assert!(
            log().contains("Top Canyon (#599) (rated best Jungle), Jungle Kiin (#598) (rated best Top)") && log().contains("NOT every player"),
            "{}",
            log()
        );
        athletes::clear();
        lanes::clear();
    }

    #[test]
    fn the_game_keeps_its_seating_when_the_lock_has_nothing_to_hold() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        config::set(config::Config::default());
        let table = ["x1", "x2", "x3", "x4", "x5", "league_garen"];
        // nothing known about any of them
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &table[..5], &[], &LANES);
        assert_eq!(LockHook.decide_positions(&draft.ctx()), None);
        assert!(log().contains(
            "picks in pick order: x1 (any), x2 (any), x3 (any), x4 (any), x5 (any) - nothing is known about where \
             these champions play: the game seats them itself"
        ));
        // one known champion is enough to seat the team: garen goes top whatever his pick slot
        let picks = ["x1", "league_garen", "x3", "x4", "x5"];
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &picks, &[], &LANES);
        assert_eq!(LockHook.decide_positions(&draft.ctx()), Some(vec![1, 0, 2, 3, 4]));
        // not the five positions (another game mode), or an older host's context
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &picks, &[], &LANES[..3]);
        assert_eq!(LockHook.decide_positions(&draft.ctx()), None);
        let mut old = FakeDraft::new(DraftPhaseV1::Position, &table, &picks, &[], &LANES);
        old.raw.size = std::mem::offset_of!(DraftCtxV1, ally_athletes_ptr);
        assert_eq!(LockHook.decide_positions(&old.ctx()), None, "no lane slots before ABI level 10");
        // the AI switch off: the game decides
        config::set(config::Config { ai: false, ..config::Config::default() });
        let draft = FakeDraft::new(DraftPhaseV1::Position, &table, &picks, &[], &LANES);
        assert_eq!(LockHook.decide_positions(&draft.ctx()), None);
        config::set(config::Config::default());
        lanes::clear();
    }

    #[test]
    fn an_ai_pick_only_has_to_fit_the_team() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        config::set(config::Config::default());
        let table = ["league_ahri", "league_garen", "league_darius", "league_viego", "league_syndra"];
        // a mid laner picked first: a top laner may come second (the team is seated at the end)
        let draft = FakeDraft::new(DraftPhaseV1::Pick, &table, &["league_ahri"], &table[1..], &[]);
        let ctx = draft.ctx();
        let score = |c: usize| LockHook.score_pick(&ctx, c, 1.0);
        assert_eq!(score(1), StableDraftDecision::Pass, "garen for top");
        assert_eq!(score(3), StableDraftDecision::Pass, "viego for the jungle");
        assert_eq!(score(4), StableDraftDecision::Replace(LOCKED_SCORE), "ahri holds mid");
        // garen holds top: darius is ruled out
        let draft = FakeDraft::new(DraftPhaseV1::Pick, &table, &["league_ahri", "league_garen"], &table[2..], &[]);
        let ctx = draft.ctx();
        assert_eq!(LockHook.score_pick(&ctx, 2, 1.0), StableDraftDecision::Replace(LOCKED_SCORE));
        assert_eq!(LockHook.score_pick(&ctx, 3, 1.0), StableDraftDecision::Pass);
        assert!(log().contains("team so far: league_ahri, league_garen (open: Jungle/Bottom/Support)"), "{}", log());
        lanes::clear();
    }
}
