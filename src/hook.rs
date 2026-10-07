//! The AI's picks. A team's picks fill its players' slots in position order (top, jungle, mid,
//! bottom, support - as the ban/pick screen shows them), so its `k`-th pick goes to the player in
//! the `k`-th position. The game scores every candidate itself, then asks the draft hooks; a
//! candidate that cannot play that position (`lanes::pickable_for`) is scored out of reach, which
//! no other hook's nudge can undo. Bans are never touched, and nothing is when every candidate on
//! offer is ruled out - the draft never gets stuck. The game's own choice among the rest (and
//! other mods' nudges to it) is left alone.
//!
//! What the game hands over and what the lock did goes to `diag.log` (`[draft]` lines): the first
//! calls in full, then one line per AI pick, so it can be checked after a draft.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{PoisonError, RwLock};

use mod_api_stable::{StableDraftContext, StableDraftDecision, StableDraftHook};

use crate::{config, diag, lanes};

pub struct LockHook;

/// The score of a pick the lock rules out: below anything the game gives.
pub const LOCKED_SCORE: f32 = -1000.0;
/// `[draft]` lines per game session at most: while the ban/pick screen is open (the player's own
/// match), and from drafts simulated off screen (the other matches of a match day).
const MAX_LIVE_LINES: u32 = 400;
const MAX_OFF_SCREEN_LINES: u32 = 30;

static LIVE_LINES: AtomicU32 = AtomicU32::new(0);
static OFF_SCREEN_LINES: AtomicU32 = AtomicU32::new(0);
/// The ban/pick screen is open (set by `screen`): the drafts now are the player's match.
static LIVE: AtomicBool = AtomicBool::new(false);
/// The first hook call of this ban/pick screen was logged.
static LIVE_FIRST: AtomicBool = AtomicBool::new(false);

/// The ban/pick screen opened or closed.
pub fn set_live(live: bool) {
    if LIVE.swap(live, Ordering::Relaxed) != live && live {
        LIVE_FIRST.store(false, Ordering::Relaxed);
    }
}

fn log_draft(msg: &str) {
    let (count, max) = if LIVE.load(Ordering::Relaxed) {
        (&LIVE_LINES, MAX_LIVE_LINES)
    } else {
        (&OFF_SCREEN_LINES, MAX_OFF_SCREEN_LINES)
    };
    if count.fetch_add(1, Ordering::Relaxed) < max {
        let place = if LIVE.load(Ordering::Relaxed) { "" } else { "(off screen) " };
        diag::log(&format!("[draft] {place}{msg}"));
    }
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
        let Some(view) = View::read(ctx) else { return Self { key: Some(key), ok: None } };
        let offer = view.names();
        let ally = view.ally();
        let ok: Vec<&str> = if ally.len() >= 5 { offer.clone() } else { lanes::pickable_for(cfg, &ally, &offer, ally.len()) };
        log_decision(&view, &ok);
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
}

static FIRST_BAN: AtomicBool = AtomicBool::new(false);
static FIRST_PICK: AtomicBool = AtomicBool::new(false);

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
        format!(
            "[draft] first {what}: phase {:?}, explore {}, {} champion names (e.g. {}), {} on offer, team picks {}, enemy picks {}",
            ctx.phase(),
            ctx.is_explore(),
            names,
            sample.join(", "),
            ctx.available_champions().len(),
            ctx.ally_picks().len(),
            ctx.enemy_picks().len()
        )
    });
}

fn log_decision(view: &View, ok: &[&str]) {
    let out: Vec<&str> = view.names().into_iter().filter(|n| !ok.contains(n)).collect();
    let team = if view.ally.is_empty() { "nobody yet".to_string() } else { view.ally.join(", ") };
    let ruled = if out.is_empty() {
        "nothing ruled out".to_string()
    } else {
        let shown: Vec<&str> = out.iter().take(8).copied().collect();
        format!("ruled out {}: {}{}", out.len(), shown.join(", "), if out.len() > 8 { ", ..." } else { "" })
    };
    let lane = lanes::ROLES.get(view.ally.len()).copied().unwrap_or("-");
    log_draft(&format!(
        "AI pick {} (for its {lane} player), team so far: {team}; {} on offer, {} fit, {ruled}",
        view.ally.len() + 1,
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
}
