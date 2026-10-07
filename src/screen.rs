//! The ban/pick screen: the player's own picks are held to the lock, and the cards teach the
//! champions' main positions.
//!
//! What the game shows (seen in its UI tree): the grid `main.champions.contents` holds one card
//! per champion, **named by the champion id**. A card's `blue` / `red` badge is visible once
//! that side picked it, its `ban` icon once it is banned, its `fearless_x` once Fearless rules
//! lock it; its `pos_tooltip.row1/row2.text` hold the champion's main positions as the game's
//! text references (`#asset/base/text/ui?position.top`). The header's `main.header.step` says
//! the phase (`...pick_phase` / `...ban_phase`); the team names sit in
//! `main.bottom.<side>_side.name` with the league rank appended ("Samsung Galaxy #1").
//!
//! In the pick phase, every open card the player's team cannot seat gets a dimmed layer with a
//! lock ([`LAYER`]); the layer is a button, so it takes the click and the card cannot be picked.
//! Nothing is locked while the player's side is not known. Other mods (Patch Meta AI) read the
//! layer to skip locked cards.

use std::collections::{HashMap, HashSet};

use mod_api_stable::StableClient;

use crate::config::Config;
use crate::{diag, lanes};

pub const GRID: &str = "main.champions.contents";
/// The layer over a card the lock rules out (a child of the card).
pub const LAYER: &str = "spl_lock";
const HEADER_STEP: &str = "main.header.step";
const TEAM_NAMES: [&str; 2] = ["main.bottom.blue_side.name", "main.bottom.red_side.name"];
const POSITION_REF: &str = "#asset/base/text/ui?position.";
/// Frames between two reads of the grid (about 130 cards, a few node reads each).
const READ_EVERY: u64 = 10;

/// The UI calls the lock needs (the game's client context, or a test double).
pub trait Ui {
    fn exists(&self, path: &str) -> bool;
    fn children(&self, path: &str) -> Vec<String>;
    fn text(&self, path: &str) -> Option<String>;
    fn visible(&self, path: &str) -> Option<bool>;
    fn spawn(&mut self, parent: &str, source: &str) -> bool;
    fn set_visible(&mut self, path: &str, visible: bool) -> bool;
}

impl Ui for StableClient<'_> {
    fn exists(&self, path: &str) -> bool {
        self.ui_exists(path)
    }
    fn children(&self, path: &str) -> Vec<String> {
        self.ui_child_names(path)
    }
    fn text(&self, path: &str) -> Option<String> {
        self.ui_text(path)
    }
    fn visible(&self, path: &str) -> Option<bool> {
        self.ui_visible(path)
    }
    fn spawn(&mut self, parent: &str, source: &str) -> bool {
        self.ui_spawn_source(parent, source)
    }
    fn set_visible(&mut self, path: &str, visible: bool) -> bool {
        self.ui_set_visible(path, visible)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub path: String,
    pub champ: String,
    pub blue: bool,
    pub red: bool,
    pub banned: bool,
    /// Fearless rules lock it.
    pub fearless: bool,
}

impl Card {
    /// Still on offer: not picked, banned or locked by Fearless.
    pub fn open(&self) -> bool {
        !self.blue && !self.red && !self.banned && !self.fearless
    }
}

/// A team name as a key: lower case, without the league rank the ban/pick screen appends
/// ("Samsung Galaxy #1") or stray line breaks.
pub fn team_key(name: &str) -> String {
    let clean: String = name.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    let mut key = clean.trim();
    if let Some(at) = key.rfind(" #") {
        if at + 2 < key.len() && key[at + 2..].chars().all(|c| c.is_ascii_digit()) {
            key = key[..at].trim_end();
        }
    }
    key.to_lowercase()
}

/// The lock layer: the card dimmed, a lock in the middle; a button, so it takes the click.
fn layer_source() -> String {
    format!(
        "{LAYER}:color_icon_button {{ width: 100%; height: 100%; btn: {{ color: #07080bb8; }} \
         hover: {{ btn: {{ color: #07080bb8; }} }} \
         #icon:image {{ width: 24px; height: 24px; anchor_x: 0.5; anchor_y: 0.5; pivot_x: 0.5; pivot_y: 0.38; \
         source: \"asset/base/ui/icons/lock\"; color: #c2c6ceff; ignore_event: true; }} }}"
    )
}

#[derive(Default)]
pub struct Screen {
    on: bool,
    next_read: u64,
    /// Champions whose card positions were read this session.
    learned: HashSet<String>,
    unsaved: bool,
    /// Layers as last written: covering or not.
    covered: HashMap<String, bool>,
    reported: bool,
    /// Each side's picks as last checked (see [`Screen::check`]).
    checked: [Vec<String>; 2],
    /// The players' slot positions were logged for this ban/pick screen.
    slots_logged: bool,
}

impl Screen {
    /// One frame. `team_name` is the player's team, `known` tells champion ids from other nodes.
    pub fn tick(&mut self, ui: &mut impl Ui, frame: u64, team_name: &str, known: &dyn Fn(&str) -> bool, cfg: &Config) {
        if !ui.exists(GRID) {
            self.close();
            return;
        }
        if !self.on {
            self.on = true;
            crate::hook::set_live(true);
            self.next_read = 0;
            diag::log("[ui] ban/pick screen");
        }
        if frame < self.next_read {
            return;
        }
        self.next_read = frame + READ_EVERY;
        let cards = read_grid(ui, known);
        self.learn(ui, &cards);

        self.check(cfg, &cards);
        if !self.slots_logged {
            self.slots_logged = log_slots(ui);
        }
        let side = player_side(ui, team_name);
        let pick_phase = ui.text(HEADER_STEP).is_some_and(|t| t.contains("pick_phase"));
        if !self.reported && !cards.is_empty() {
            self.reported = true;
            diag::log(&format!(
                "[ui] grid: {} cards; player side {}; main positions known for {} champions",
                cards.len(),
                match side {
                    Some(0) => "blue",
                    Some(1) => "red",
                    _ => "unknown (nothing is locked)",
                },
                lanes::known()
            ));
        }
        let locking = cfg.enabled && cfg.player && pick_phase;
        let ruled_out: HashSet<&str> = match side {
            Some(side) if locking => {
                let team: Vec<&str> =
                    cards.iter().filter(|c| if side == 0 { c.blue } else { c.red }).map(|c| c.champ.as_str()).collect();
                let open: Vec<&str> = cards.iter().filter(|c| c.open()).map(|c| c.champ.as_str()).collect();
                if team.len() >= 5 {
                    HashSet::new()
                } else {
                    let ok: HashSet<&str> = lanes::pickable(cfg, &team, &open).into_iter().collect();
                    open.into_iter().filter(|c| !ok.contains(c)).collect()
                }
            }
            _ => HashSet::new(),
        };
        for c in &cards {
            let cover = ruled_out.contains(c.champ.as_str());
            let layer = format!("{}.{LAYER}", c.path);
            if cover && !ui.exists(&layer) {
                // a new layer, or the game rebuilt the card
                if !ui.spawn(&c.path, &layer_source()) {
                    diag::log_once("layer", "[ui] could not add the lock layer to a card");
                    continue;
                }
                self.covered.insert(layer.clone(), true);
            }
            if self.covered.get(&layer).copied().unwrap_or(false) != cover && ui.exists(&layer) {
                ui.set_visible(&layer, cover);
            }
            if cover || self.covered.contains_key(&layer) {
                self.covered.insert(layer, cover);
            }
        }
    }

    /// The ban/pick screen closed (or the game was left from it): positions learned on it are
    /// saved, and the hook no longer logs drafts as the player's.
    pub fn close(&mut self) {
        if !self.on {
            return;
        }
        self.on = false;
        crate::hook::set_live(false);
        self.covered.clear();
        self.checked = Default::default();
        self.slots_logged = false;
        if std::mem::take(&mut self.unsaved) {
            lanes::save();
        }
    }

    /// Whether each side's picks can still be seated one per position - for the AI's picks the
    /// proof that the draft hook holds the lock. Written to `diag.log` whenever a side picks.
    fn check(&mut self, cfg: &Config, cards: &[Card]) {
        if !cfg.enabled {
            return;
        }
        let played = lanes::played();
        for (side, name) in ["blue", "red"].iter().enumerate() {
            let picks: Vec<String> =
                cards.iter().filter(|c| if side == 0 { c.blue } else { c.red }).map(|c| c.champ.clone()).collect();
            if picks == self.checked[side] {
                continue;
            }
            self.checked[side] = picks.clone();
            if picks.is_empty() {
                continue;
            }
            let lanes: Vec<lanes::Lanes> = picks.iter().map(|c| lanes::allowed(c, cfg, played.as_deref())).collect();
            let seated = lanes::fits(&lanes);
            let detail: Vec<String> = picks
                .iter()
                .zip(&lanes)
                .map(|(c, l)| {
                    let at: Vec<&str> = lanes::ROLES.iter().enumerate().filter(|(i, _)| l[*i]).map(|(_, r)| *r).collect();
                    format!("{c} ({})", if at.len() == 5 { "any".to_string() } else { at.join("/") })
                })
                .collect();
            diag::log(&format!(
                "[check] {name} picks {}: {}",
                detail.join(", "),
                if seated { "fit one per position" } else { "DO NOT fit one per position - the lock did not hold" }
            ));
        }
    }

    /// The main positions on the cards, read once per champion and session; saved when the ban/pick
    /// screen closes.
    fn learn(&mut self, ui: &impl Ui, cards: &[Card]) {
        for c in cards {
            if self.learned.contains(&c.champ) {
                continue;
            }
            let mut found = [false; 5];
            for row in ["row1", "row2"] {
                let text = ui.text(&format!("{}.pos_tooltip.{row}.text", c.path)).unwrap_or_default();
                if let Some(r) = text.strip_prefix(POSITION_REF).and_then(lanes::parse_role) {
                    found[r] = true;
                }
            }
            if found == [false; 5] {
                // the tooltip is not filled yet: look again on the next read
                continue;
            }
            self.learned.insert(c.champ.clone());
            if lanes::learn(&c.champ, found) {
                self.unsaved = true;
            }
        }
    }
}

/// The position of each player's pick slot, as the slot's player card says - a team's picks fill
/// them in order, and the AI lock assumes the order top, jungle, mid, bottom, support. Logged once
/// per ban/pick screen; true once read.
fn log_slots(ui: &impl Ui) -> bool {
    let mut sides = Vec::new();
    for side in ["blue", "red"] {
        let lanes: Vec<Option<usize>> = (0..5)
            .map(|k| {
                ui.text(&format!("main.{side}_picks.pick_slot_{k}.popup.header.position_name"))
                    .and_then(|t| t.strip_prefix(POSITION_REF).and_then(lanes::parse_role))
            })
            .collect();
        if lanes.iter().any(Option::is_none) {
            return false;
        }
        let standard = lanes.iter().enumerate().all(|(k, l)| *l == Some(k));
        let names: Vec<&str> = lanes.iter().flatten().map(|l| lanes::ROLES[*l]).collect();
        sides.push(format!("{side} {}{}", names.join(","), if standard { "" } else { " (NOT the usual order: AI picks may go to the wrong player)" }));
    }
    diag::log(&format!("[ui] pick slots: {}", sides.join("; ")));
    true
}

fn shown(ui: &impl Ui, path: &str) -> bool {
    ui.visible(path) == Some(true)
}

pub fn read_grid(ui: &impl Ui, known: &dyn Fn(&str) -> bool) -> Vec<Card> {
    ui.children(GRID)
        .into_iter()
        .filter(|child| known(child))
        .map(|child| {
            let path = format!("{GRID}.{child}");
            Card {
                blue: shown(ui, &format!("{path}.blue")),
                red: shown(ui, &format!("{path}.red")),
                banned: shown(ui, &format!("{path}.ban")),
                fearless: shown(ui, &format!("{path}.fearless_x")),
                champ: child,
                path,
            }
        })
        .collect()
}

/// 0 = the player is blue, 1 = red (from the team names at the bottom of the screen).
pub fn player_side(ui: &impl Ui, team_name: &str) -> Option<usize> {
    let team = team_key(team_name);
    if team.is_empty() {
        return None;
    }
    TEAM_NAMES.iter().position(|p| ui.text(p).is_some_and(|t| team_key(&t) == team))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::lanes::tests::{MID_SUP, TOP};
    use std::collections::BTreeMap;

    /// A UI tree in memory: nodes by path, with text and visibility.
    #[derive(Default)]
    pub struct FakeUi {
        pub nodes: BTreeMap<String, (Option<String>, bool)>,
        pub spawns: usize,
    }

    impl FakeUi {
        pub fn add(&mut self, path: &str, text: Option<&str>) {
            self.nodes.insert(path.to_string(), (text.map(str::to_string), true));
        }
        fn hide(&mut self, path: &str) {
            if let Some(n) = self.nodes.get_mut(path) {
                n.1 = false;
            }
        }
    }

    impl Ui for FakeUi {
        fn exists(&self, path: &str) -> bool {
            self.nodes.contains_key(path)
        }
        fn children(&self, path: &str) -> Vec<String> {
            let prefix = format!("{path}.");
            self.nodes.keys().filter_map(|k| k.strip_prefix(&prefix)).filter(|r| !r.contains('.')).map(str::to_string).collect()
        }
        fn text(&self, path: &str) -> Option<String> {
            self.nodes.get(path)?.0.clone()
        }
        fn visible(&self, path: &str) -> Option<bool> {
            Some(self.nodes.get(path)?.1)
        }
        fn spawn(&mut self, parent: &str, source: &str) -> bool {
            if !self.nodes.contains_key(parent) {
                return false;
            }
            let name = source.split(':').next().unwrap_or_default();
            self.add(&format!("{parent}.{name}"), None);
            self.add(&format!("{parent}.{name}.icon"), None);
            self.spawns += 1;
            true
        }
        fn set_visible(&mut self, path: &str, visible: bool) -> bool {
            match self.nodes.get_mut(path) {
                Some(n) => {
                    n.1 = visible;
                    true
                }
                None => false,
            }
        }
    }

    const CHAMPS: [&str; 6] = ["a", "b", "c", "d", "e", "f"];

    /// The ban/pick screen as the game builds it: cards named by champion id with hidden
    /// badges and the main positions in the tooltip, team names with a rank.
    pub fn screen(positions: &[(&str, &[&str])]) -> FakeUi {
        let mut ui = FakeUi::default();
        ui.add("main", None);
        ui.add(GRID, None);
        ui.add("main.champions.contents.scrollbar", None);
        for c in CHAMPS {
            let card = format!("{GRID}.{c}");
            ui.add(&card, None);
            for part in ["blue", "red", "ban", "fearless_x"] {
                ui.add(&format!("{card}.{part}"), None);
                ui.hide(&format!("{card}.{part}"));
            }
            let rows = positions.iter().find(|(n, _)| *n == c).map(|(_, r)| *r).unwrap_or(&[]);
            for (i, row) in ["row1", "row2"].iter().enumerate() {
                let text = rows.get(i).map(|p| format!("{POSITION_REF}{p}"));
                ui.add(&format!("{card}.pos_tooltip.{row}.text"), text.as_deref());
            }
        }
        ui.add(TEAM_NAMES[0], Some("Mods FC #3"));
        ui.add(TEAM_NAMES[1], Some("Rivals\r #1"));
        ui.add(HEADER_STEP, Some("#asset/base/text/ui?banpick.pick_phase"));
        ui
    }

    fn known(c: &str) -> bool {
        CHAMPS.contains(&c)
    }

    fn locked(ui: &FakeUi) -> Vec<&'static str> {
        CHAMPS.into_iter().filter(|c| ui.visible(&format!("{GRID}.{c}.{LAYER}")) == Some(true)).collect()
    }

    #[test]
    fn team_keys() {
        assert_eq!(team_key("Samsung Galaxy #1"), "samsung galaxy");
        assert_eq!(team_key("Jin Air Green Wings\r #10"), "jin air green wings");
        assert_eq!(team_key("Team #Blue"), "team #blue");
    }

    #[test]
    fn locks_the_cards_the_team_cannot_seat() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        let cfg = Config { history: false, ..Config::default() };
        let mut ui = screen(&[("a", &["top"]), ("b", &["top"]), ("c", &["mid", "support"]), ("d", &["jungle", "top"])]);
        let mut s = Screen::default();
        s.tick(&mut ui, 0, "Mods FC", &known, &cfg);
        assert_eq!(lanes::main_of("a"), Some(TOP), "main positions learned from the cards");
        assert_eq!(lanes::main_of("c"), Some(MID_SUP));
        assert_eq!(lanes::main_of("e"), None, "no positions on its card");
        assert!(locked(&ui).is_empty(), "nothing picked yet");

        // the player (blue) picks a: b (top only) is locked, d moves to the jungle
        ui.set_visible(&format!("{GRID}.a.blue"), true);
        s.tick(&mut ui, 10, "Mods FC", &known, &cfg);
        assert_eq!(locked(&ui), ["b"]);
        // the enemy's picks do not count for the player
        ui.set_visible(&format!("{GRID}.c.red"), true);
        s.tick(&mut ui, 20, "Mods FC", &known, &cfg);
        assert_eq!(locked(&ui), ["b"]);
        // b is banned: no layer on a card that is not on offer
        ui.set_visible(&format!("{GRID}.b.ban"), true);
        s.tick(&mut ui, 30, "Mods FC", &known, &cfg);
        assert!(locked(&ui).is_empty());
        ui.set_visible(&format!("{GRID}.b.ban"), false);

        // the ban phase: nothing locked
        ui.add(HEADER_STEP, Some("#asset/base/text/ui?banpick.ban_phase"));
        s.tick(&mut ui, 40, "Mods FC", &known, &cfg);
        assert!(locked(&ui).is_empty());
        ui.add(HEADER_STEP, Some("#asset/base/text/ui?banpick.pick_phase"));
        s.tick(&mut ui, 50, "Mods FC", &known, &cfg);
        assert_eq!(locked(&ui), ["b"]);

        // the game rebuilt the card: the layer comes back
        ui.nodes.retain(|k, _| !k.contains(LAYER));
        s.tick(&mut ui, 60, "Mods FC", &known, &cfg);
        assert_eq!(locked(&ui), ["b"]);

        // an unknown side or the setting off: nothing locked
        s.tick(&mut ui, 70, "Somebody", &known, &cfg);
        assert!(locked(&ui).is_empty());
        let off = Config { player: false, ..cfg.clone() };
        s.tick(&mut ui, 80, "Mods FC", &known, &off);
        assert!(locked(&ui).is_empty());

        // the screen closes: the positions are saved
        ui.nodes.retain(|k, _| !k.starts_with(GRID));
        s.tick(&mut ui, 90, "Mods FC", &known, &cfg);
        lanes::clear();
        lanes::load();
        assert_eq!(lanes::main_of("d"), Some([true, true, false, false, false]));
        lanes::clear();
    }

    #[test]
    fn the_check_says_whether_a_team_fits() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        lanes::clear();
        let cfg = Config { history: false, ..Config::default() };
        let mut ui = screen(&[("a", &["top"]), ("b", &["top"]), ("c", &["mid"])]);
        let mut s = Screen::default();
        ui.set_visible(&format!("{GRID}.a.red"), true);
        ui.set_visible(&format!("{GRID}.c.red"), true);
        s.tick(&mut ui, 0, "Mods FC", &known, &cfg);
        ui.set_visible(&format!("{GRID}.b.red"), true);
        s.tick(&mut ui, 10, "Mods FC", &known, &cfg);
        let log = std::fs::read_to_string(crate::paths::mod_dir().join(crate::diag::LOG_FILE)).unwrap();
        assert!(log.contains("[check] red picks a (Top), c (Mid): fit one per position"), "{log}");
        assert!(log.contains("[check] red picks a (Top), b (Top), c (Mid): DO NOT fit"), "{log}");
        lanes::clear();
    }

    #[test]
    fn the_red_side_and_no_fitting_champion() {
        let _serial = crate::tests::serial();
        lanes::clear();
        let cfg = Config { history: false, ..Config::default() };
        let mut ui = screen(&[("a", &["top"]), ("b", &["top"]), ("c", &["top"]), ("d", &["top"]), ("e", &["top"]), ("f", &["top"])]);
        let mut s = Screen::default();
        // the player is red and picked a; every other champion is top only: nothing fits, so
        // nothing is locked (the draft must go on)
        ui.set_visible(&format!("{GRID}.a.red"), true);
        s.tick(&mut ui, 0, "rivals", &known, &cfg);
        assert!(locked(&ui).is_empty());
        lanes::clear();
    }
}
