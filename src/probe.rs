//! A temporary probe: where the game keeps the line-ups of the match being played. The AI
//! reorders its champions in the swap phase, after the draft hook; to put them back, the lock
//! needs to know whether the swap result lives in a record it can rewrite before the match.
//!
//! On the ban/pick screen the match-day records (`MatchNormal`) are scanned for the one in
//! progress (its `running_state` neither `Wait` nor `End`); that record, and the newest `Match`
//! records, are written to `probe_live_match.txt` at the start of the draft, during the swap
//! phase and once the swap phase is over.

use std::fmt::Write as _;

use mod_api_stable::RecordKindV1;

use crate::diag;
use crate::history::Source;

pub const FILE: &str = "probe_live_match.txt";
/// `running_state` reads per frame while scanning.
const SCAN_PER_FRAME: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Draft,
    Swap,
    AfterSwap,
}

#[derive(Default)]
pub struct Probe {
    /// `MatchNormal` ids still to look at, and the ones found in progress.
    to_scan: Vec<usize>,
    scanning: bool,
    live: Vec<usize>,
    done: Vec<Stage>,
    out: String,
}

impl Probe {
    /// One frame on the ban/pick screen; `swap` = the swap phase is showing.
    pub fn tick(&mut self, src: &mut impl Source, on_screen: bool, swap: bool) {
        if !on_screen {
            if self.done.contains(&Stage::Swap) && !self.done.contains(&Stage::AfterSwap) {
                self.dump(src, Stage::AfterSwap);
            }
            if !self.out.is_empty() {
                diag::write_file(FILE, &self.out);
            }
            *self = Self::default();
            return;
        }
        if !self.scanning && self.done.is_empty() && self.live.is_empty() && self.to_scan.is_empty() {
            self.scanning = true;
            self.to_scan = src.record_ids(RecordKindV1::MatchNormal);
            diag::log(&format!("[probe] scanning {} match-day records for the match in progress", self.to_scan.len()));
        }
        if self.scanning {
            for _ in 0..SCAN_PER_FRAME {
                let Some(id) = self.to_scan.pop() else {
                    self.scanning = false;
                    diag::log(&format!("[probe] match-day records in progress: {:?}", self.live));
                    break;
                };
                let state = src.record_json(RecordKindV1::MatchNormal, id, "running_state").unwrap_or_default();
                let idle = state.contains("Wait") || state.contains("End");
                if !idle && !state.is_empty() && state != "null" {
                    self.live.push(id);
                }
            }
            if self.scanning {
                return;
            }
        }
        let stage = if swap { Stage::Swap } else if self.done.contains(&Stage::Swap) { Stage::AfterSwap } else { Stage::Draft };
        if !self.done.contains(&stage) {
            self.dump(src, stage);
            diag::write_file(FILE, &self.out);
        }
    }

    fn dump(&mut self, src: &mut impl Source, stage: Stage) {
        self.done.push(stage);
        let _ = writeln!(self.out, "########## {stage:?}");
        for id in &self.live {
            let json = src.record_json(RecordKindV1::MatchNormal, *id, "").unwrap_or_default();
            let _ = writeln!(self.out, "===== MatchNormal #{id}\n{json}\n");
        }
        let mut ids = src.record_ids(RecordKindV1::Match);
        ids.sort_unstable();
        let _ = writeln!(self.out, "===== Match records: {} ids, newest {:?}", ids.len(), ids.iter().rev().take(3).collect::<Vec<_>>());
        for id in ids.iter().rev().take(2) {
            let json = src.record_json(RecordKindV1::Match, *id, "").unwrap_or_default();
            let _ = writeln!(self.out, "===== Match #{id}\n{}\n", clip(&json, 200_000));
        }
        diag::log(&format!("[probe] {stage:?}: wrote {} match-day records and the newest Match records to {FILE}", self.live.len()));
    }
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
