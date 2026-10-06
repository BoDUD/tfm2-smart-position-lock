//! The AI's picks. The game scores every candidate itself, then asks the draft hooks; a
//! candidate the team cannot seat in an open position (`lanes::pickable`) is scored out of reach.
//! As a safety net the hook also decides the pick when the game's best-scored candidate does not
//! fit: the best-scored one that does is taken instead. Bans are never touched, and nothing is
//! when every candidate on offer is ruled out - the draft never gets stuck.
//!
//! What the game hands over and what the lock did goes to `diag.log` (`[draft]` lines): the first
//! call in full, then one line per AI pick, so it can be checked after a draft.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mod_api_stable::{StableDraftContext, StableDraftDecision, StableDraftHook};

use crate::{config, diag, lanes};

pub struct LockHook;

/// The score of a pick the lock rules out: below anything the game gives.
pub const LOCKED_SCORE: f32 = -1000.0;
/// `[draft]` lines per game session at most.
const MAX_LOG_LINES: u32 = 120;

static LOG_LINES: Mutex<u32> = Mutex::new(0);

fn log_draft(msg: &str) {
    let mut n = LOG_LINES.lock().unwrap_or_else(PoisonError::into_inner);
    if *n < MAX_LOG_LINES {
        *n += 1;
        diag::log(&format!("[draft] {msg}"));
    }
}

/// One pick decision as the game describes it: the team's picks and the candidates, by name.
struct View<'a> {
    ally: Vec<&'a str>,
    offer: Vec<(usize, &'a str)>,
}

impl<'a> View<'a> {
    fn read(ctx: &StableDraftContext<'a>) -> Option<Self> {
        let ally: Option<Vec<&str>> = ctx.ally_picks().iter().map(|id| ctx.champion_name(*id)).collect();
        let offer: Option<Vec<(usize, &str)>> =
            ctx.available_champions().iter().map(|id| Some((*id, ctx.champion_name(*id)?))).collect();
        match (ally, offer) {
            (Some(ally), Some(offer)) => Some(Self { ally, offer }),
            _ => {
                diag::log_once(
                    "draft-names",
                    "[draft] the game gave champion ids without names: the lock cannot judge the AI's picks",
                );
                None
            }
        }
    }

    fn key(&self) -> String {
        format!("{}|{}", self.ally.join(","), self.offer.len())
    }

    fn names(&self) -> Vec<&'a str> {
        self.offer.iter().map(|(_, n)| *n).collect()
    }
}

/// The decision in progress on this thread: the game scores every candidate of one decision in a
/// row (hooks may run on several threads).
#[derive(Default)]
struct Decision {
    key: String,
    rules: (u64, usize),
    /// Which of the offer the team may pick.
    ok: Vec<String>,
    /// The game's own score of each candidate scored so far.
    scores: HashMap<usize, f32>,
    logged: bool,
}

thread_local! {
    static CURRENT: std::cell::RefCell<Decision> = std::cell::RefCell::new(Decision::default());
}

/// Brings the thread's decision up to date with `view` (a new decision when the team or the offer
/// changed, or the rules did). Returns whether this is the first look at it.
fn with_decision<R>(cfg: &config::Config, view: &View<'_>, f: impl FnOnce(&mut Decision, bool) -> R) -> R {
    CURRENT.with(|cell| {
        let mut d = cell.borrow_mut();
        let key = view.key();
        let rules = (lanes::generation(), cfg as *const config::Config as usize);
        let fresh = d.key != key || d.rules != rules;
        if fresh {
            let offer = view.names();
            let ok = if view.ally.len() >= 5 { offer.clone() } else { lanes::pickable(cfg, &view.ally, &offer) };
            *d = Decision { key, rules, ok: ok.into_iter().map(str::to_string).collect(), ..Decision::default() };
        }
        f(&mut d, fresh)
    })
}

impl StableDraftHook for LockHook {
    fn id(&self) -> String {
        format!("{}:lock", crate::MOD_ID)
    }

    fn score_ban(&self, ctx: &StableDraftContext<'_>, _candidate: usize, _base: f32) -> StableDraftDecision {
        first_call(ctx, "score_ban");
        StableDraftDecision::Pass
    }

    fn score_pick(&self, ctx: &StableDraftContext<'_>, candidate: usize, base: f32) -> StableDraftDecision {
        first_call(ctx, "score_pick");
        let cfg = config::get();
        if !cfg.enabled || !cfg.ai {
            return StableDraftDecision::Pass;
        }
        let Some(view) = View::read(ctx) else { return StableDraftDecision::Pass };
        let Some(cand) = ctx.champion_name(candidate) else { return StableDraftDecision::Pass };
        with_decision(&cfg, &view, |d, _| {
            d.scores.insert(candidate, base);
            if !d.logged {
                d.logged = true;
                log_decision(&view, &d.ok);
            }
            // a candidate not on offer (should not happen) is left to the game
            if !view.offer.iter().any(|(_, n)| *n == cand) || d.ok.iter().any(|c| c == cand) {
                StableDraftDecision::Pass
            } else {
                StableDraftDecision::Replace(LOCKED_SCORE)
            }
        })
    }

    fn decide_pick(&self, ctx: &StableDraftContext<'_>) -> Option<usize> {
        first_call(ctx, "decide_pick");
        let cfg = config::get();
        if !cfg.enabled || !cfg.ai {
            return None;
        }
        let view = View::read(ctx)?;
        with_decision(&cfg, &view, |d, fresh| {
            if fresh || d.scores.is_empty() {
                diag::log_once("draft-order", "[draft] the game asks for a decision before the scores: the lock works through the scores");
                return None;
            }
            diag::log_once("draft-order", "[draft] the game asks for a decision after the scores: the lock also decides when needed");
            let pick = choose(&view, &d.ok, &d.scores)?;
            log_draft(&format!(
                "the game's best-scored candidate does not fit: the lock picks {} instead",
                view.offer.iter().find(|(id, _)| *id == pick).map_or("?", |(_, n)| n)
            ));
            Some(pick)
        })
    }
}

/// The pick the lock makes: `None` when the game's best-scored candidate fits (the game picks),
/// else the best-scored candidate that fits.
fn choose(view: &View<'_>, ok: &[String], scores: &HashMap<usize, f32>) -> Option<usize> {
    let fits = |id: usize| view.offer.iter().any(|(i, n)| *i == id && ok.iter().any(|c| c == n));
    let best = |only_fitting: bool| {
        scores
            .iter()
            .filter(|(id, _)| !only_fitting || fits(**id))
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| *id)
    };
    let overall = best(false)?;
    if fits(overall) {
        None
    } else {
        best(true)
    }
}

/// The first hook call of the session, in full: what the game hands over.
fn first_call(ctx: &StableDraftContext<'_>, what: &str) {
    diag::log_once(&format!("draft-first-{what}"), &{
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

fn log_decision(view: &View<'_>, ok: &[String]) {
    let out: Vec<&str> = view.names().into_iter().filter(|n| !ok.iter().any(|c| c == n)).collect();
    let team = if view.ally.is_empty() { "nobody yet".to_string() } else { view.ally.join(", ") };
    let ruled = if out.is_empty() {
        "nothing ruled out".to_string()
    } else {
        let shown: Vec<&str> = out.iter().take(8).copied().collect();
        format!("ruled out {}: {}{}", out.len(), shown.join(", "), if out.len() > 8 { ", ..." } else { "" })
    };
    log_draft(&format!("AI pick for a team with {team}: {} on offer, {} fit, {ruled}", view.offer.len(), ok.len()));
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

    #[test]
    fn decides_only_when_the_best_scored_candidate_does_not_fit() {
        let view = View { ally: vec!["a"], offer: vec![(1, "b"), (2, "c"), (3, "d")] };
        let ok = vec!["c".to_string(), "d".to_string()];
        // the game likes b (does not fit) best: the best of c and d is taken
        let scores: HashMap<usize, f32> = [(1, 9.0), (2, 3.0), (3, 5.0)].into();
        assert_eq!(choose(&view, &ok, &scores), Some(3));
        // the game likes d best: it picks itself
        let scores: HashMap<usize, f32> = [(1, 2.0), (2, 3.0), (3, 5.0)].into();
        assert_eq!(choose(&view, &ok, &scores), None);
        // nothing scored: nothing decided
        assert_eq!(choose(&view, &ok, &HashMap::new()), None);
    }
}
