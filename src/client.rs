//! The client side, once per frame: the save's history (`history`, on the management screens
//! only), the fielded players' proficiency (`athletes`, on the management screens and the
//! ban/pick screen) and the ban/pick screen (`screen`).

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use mod_api_stable::{ClientSceneKindV1, RecordKindV1, StableClient, StableExtension};

use crate::athletes::Reader;
use crate::config;
use crate::history::{History, Source};
use crate::screen::Screen;
use crate::ui::Ui;

struct ClientSource<'a, 'b> {
    ctx: &'a mut StableClient<'b>,
}

impl Source for ClientSource<'_, '_> {
    fn record_ids(&mut self, kind: RecordKindV1) -> Vec<usize> {
        self.ctx.record_ids(kind)
    }
    fn record_json(&mut self, kind: RecordKindV1, id: usize, path: &str) -> Option<String> {
        self.ctx.record_get_json(kind, id, path)
    }
    fn player_team(&mut self) -> Option<usize> {
        self.ctx.player_team_id()
    }
    fn team_name(&mut self, team: usize) -> Option<String> {
        self.ctx.team_name(team)
    }
}

#[derive(Default)]
struct State {
    frame: u64,
    history: History,
    screen: Screen,
    panel: crate::panel::Panel,
    /// The save's champion ids (cards are named by them), and the save they were read for.
    champions: HashSet<String>,
    champions_for: Option<(usize, String)>,
    /// The fielded players' proficiency, and the save it is for.
    athletes: Reader,
    athletes_for: Option<(usize, String)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct ClientExt;

impl StableExtension for ClientExt {
    fn post_update(&self, ctx: &mut StableClient<'_>, _dt_micros: u64) {
        let mut guard = lock();
        if !ctx.is_in_game() {
            if let Some(mut st) = guard.take() {
                st.screen.close();
                st.history.reset();
                st.athletes.reset();
                crate::hook::set_player_team(None);
            }
            return;
        }
        let st = guard.get_or_insert_with(State::default);
        st.frame += 1;
        let now = Instant::now();
        config::refresh(now);
        let cfg = config::get();
        // records are read on the management screens only: not a frame around a match is spent
        let read = cfg.enabled && cfg.history && matches!(ctx.client_scene_kind(), None | Some(ClientSceneKindV1::Main));
        st.history.tick(&mut ClientSource { ctx }, now, read);
        // the seating log names the player's team
        crate::hook::set_player_team(st.history.save.as_ref().map(|s| s.0));
        // the fielded players' proficiency (athlete ids are per save); read on the management
        // screens and on the ban/pick screen, where the AI opponent's line-up is seated
        if st.athletes_for != st.history.save {
            st.athletes_for = st.history.save.clone();
            st.athletes.reset();
        }
        let scene = ctx.client_scene_kind();
        let read_athletes = cfg.enabled && cfg.ai && (matches!(scene, None | Some(ClientSceneKindV1::Main)) || st.screen.is_on());
        st.athletes.tick(&mut ClientSource { ctx }, now, read_athletes);
        // asked again for a new save, and now and then while the game has none to give
        if st.champions_for != st.history.save || (st.champions.is_empty() && st.frame.is_multiple_of(120)) {
            st.champions_for = st.history.save.clone();
            st.champions = ctx.champion_names().into_iter().collect();
        }
        let team_name = st.history.save.as_ref().map(|s| s.1.clone()).unwrap_or_default();
        let champions = &st.champions;
        let known = |c: &str| champions.contains(c);
        st.screen.tick(ctx, st.frame, &team_name, &known, &cfg);
        // the settings panel (F7), on any screen
        let keys = ctx.keys_pressed();
        let clicks = crate::ui::take_clicks();
        let scene = format!("{:?}", ctx.client_scene_kind());
        st.panel.tick(ctx, st.frame, &scene, &keys, &clicks);
    }
}
