//! The AI's picks: a candidate the team cannot seat in an open position (`lanes::pickable`) is
//! scored out of reach. Bans are never touched, and nothing is when every candidate on offer is
//! ruled out - the draft never gets stuck.

use mod_api_stable::{StableDraftContext, StableDraftDecision, StableDraftHook};

use crate::{config, lanes};

pub struct LockHook;

/// The score of a pick the lock rules out: below anything the game gives.
pub const LOCKED_SCORE: f32 = -1000.0;

impl StableDraftHook for LockHook {
    fn id(&self) -> String {
        format!("{}:lock", crate::MOD_ID)
    }

    fn score_ban(&self, _ctx: &StableDraftContext<'_>, _candidate: usize, _base: f32) -> StableDraftDecision {
        StableDraftDecision::Pass
    }

    fn score_pick(&self, ctx: &StableDraftContext<'_>, candidate: usize, _base: f32) -> StableDraftDecision {
        let cfg = config::get();
        if !cfg.enabled || !cfg.ai {
            return StableDraftDecision::Pass;
        }
        let names = |list: &[usize]| -> Vec<&str> { list.iter().filter_map(|id| ctx.champion_name(*id)).collect() };
        let Some(cand) = ctx.champion_name(candidate) else { return StableDraftDecision::Pass };
        if allows(&cfg, cand, &names(ctx.ally_picks()), &names(ctx.available_champions())) {
            StableDraftDecision::Pass
        } else {
            StableDraftDecision::Replace(LOCKED_SCORE)
        }
    }
}

/// What the answer was for (the rules' generation and settings, the team's picks, the offer),
/// and which of the offer the team may pick.
struct Pickable {
    key: (u64, usize),
    ally: Vec<String>,
    offer: Vec<String>,
    ok: Vec<String>,
}

thread_local! {
    /// The answer for the last team asked about: the game asks about every candidate of one
    /// decision in a row, with the same picks and offer (hooks may run on several threads).
    static LAST: std::cell::RefCell<Option<Pickable>> = const { std::cell::RefCell::new(None) };
}

/// Whether a team that picked `ally` may pick `cand` out of `available`.
pub fn allows(cfg: &config::Config, cand: &str, ally: &[&str], available: &[&str]) -> bool {
    if ally.len() >= 5 {
        return true;
    }
    LAST.with(|cell| {
        let mut cache = cell.borrow_mut();
        let key = (lanes::generation(), cfg as *const config::Config as usize);
        let fresh = cache.as_ref().is_some_and(|p| p.key == key && p.ally == ally && p.offer == available);
        if !fresh {
            *cache = Some(Pickable {
                key,
                ally: ally.iter().map(|s| s.to_string()).collect(),
                offer: available.iter().map(|s| s.to_string()).collect(),
                ok: lanes::pickable(cfg, ally, available).into_iter().map(str::to_string).collect(),
            });
        }
        // a candidate not on offer (should not happen) is left to the game
        cache.as_ref().is_some_and(|p| !p.offer.iter().any(|c| c == cand) || p.ok.iter().any(|c| c == cand))
    })
}

/// Test support: forget the cached answer (the rules changed under it).
#[doc(hidden)]
pub fn forget() {
    LAST.with(|cell| *cell.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::tests::{MID_SUP, TOP};

    #[test]
    fn rules_out_picks_no_open_position_can_take() {
        let _serial = crate::tests::serial();
        lanes::clear();
        forget();
        let cfg = config::Config::default();
        lanes::learn("a", TOP);
        lanes::learn("b", TOP);
        lanes::learn("c", MID_SUP);
        assert!(!allows(&cfg, "b", &["a"], &["b", "c"]), "a holds top");
        assert!(allows(&cfg, "c", &["a"], &["b", "c"]));
        assert!(allows(&cfg, "b", &["a"], &["b"]), "nothing else on offer: the draft goes on");
        assert!(allows(&cfg, "b", &["a", "c", "x", "y", "z"], &["b"]), "a full team");
        lanes::clear();
        forget();
    }
}
