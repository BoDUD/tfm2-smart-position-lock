//! The settings panel, on any screen: F7 opens and closes it; Esc, its close button or moving to
//! another screen close it too. Everything `settings.ini` holds can be set here, and every click
//! takes effect at once and is written to the file (`config::write_value` keeps the rest of it):
//!
//! - four switches: the whole lock, the AI's picks, the player's own picks, positions from the
//!   save's matches;
//! - every champion of the ban/pick grid (and any with positions of its own), 24 to a page: its
//!   face and name, the five positions - lit where it may play - and whether those are the
//!   player's own (`[positions]`, "手动", with ↺ to go back) or worked out ("自动": the card's main
//!   positions and the save's matches). A click on a position turns it on or off for that
//!   champion; it then has positions of its own. The last position cannot be turned off.
//!
//! The panel takes the clicks over its own area (a click between its buttons must not reach a
//! champion card underneath); the rest of the screen stays usable.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use crate::config::{self, Config};
use crate::lanes::{self, Lanes};
use crate::ui::{quote, Ui};

pub const HOTKEY: &str = "F7";
const NODE: &str = "spl_panel";
const CLOSE_KEYS: [&str; 2] = ["Escape", "Esc"];
const PER_PAGE: usize = 24;
const PER_COLUMN: usize = 12;
const WIDTH: u32 = 1240;
const HEIGHT: u32 = 860;
/// Frames between two looks whether the game rebuilt the screen under the panel.
const HEAL_EVERY: u64 = 20;

const PANEL: &str = "#161721f4";
const TEXT: &str = "#e8e8e8ff";
const DIM: &str = "#a3a9b6ff";
const ON: &str = "#2e8b62ff";
const OFF: &str = "#2a2d3aff";
const ON_HOVER: &str = "#38a374ff";
const OFF_HOVER: &str = "#3a3e50ff";
const SLOT: &str = "#232533ff";

/// The switches: setting key, label.
const SWITCHES: [(&str, &str); 4] =
    [("enabled", "总开关 Lock"), ("ai", "锁定 AI 选人"), ("player", "锁定我自己选人"), ("history", "参考存档战绩")];

#[derive(Default)]
pub struct Panel {
    open: bool,
    page: usize,
    /// Where it is drawn, the screen it was opened on, and what it was drawn for.
    parent: String,
    screen: String,
    drawn: Option<u64>,
    next_heal: u64,
    /// The champions of the page as drawn (rows are clicked by index).
    rows: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn label(id: &str, x: i32, y: i32, w: u32, h: u32, size: u32, color: &str, align: &str, text: &str) -> String {
    format!(
        "#{id}:label {{ @\"asset/base/style/main#label\"; x: {x}px; y: {y}px; width: {w}px; height: {h}px; size: {size}; \
         color: {color}; align_x: {align}; align_y: Center; text: {}; ignore_event: true; }} ",
        quote(text)
    )
}

/// A button: a coloured rounded area that takes the click, with a centred label.
#[allow(clippy::too_many_arguments)]
fn button(id: &str, x: i32, y: i32, w: u32, h: u32, lit: bool, size: u32, text: &str) -> String {
    let (fill, hover, ink) = if lit { (ON, ON_HOVER, TEXT) } else { (OFF, OFF_HOVER, DIM) };
    format!(
        "#{id}:color_icon_button {{ x: {x}px; y: {y}px; width: {w}px; height: {h}px; btn: {{ color: {fill}; }} \
         hover: {{ btn: {{ color: {hover}; }} }} rounding: Uniform {{ rounding: 6; }} {}}} ",
        label("t", 0, 0, w, h, size, ink, "Center", text)
    )
}

fn position_ref(lane: usize) -> String {
    format!("#asset/base/text/ui?position.{}", lanes::ROLES[lane].to_ascii_lowercase())
}

fn name_ref(champion: &str) -> String {
    format!("#asset/base/text/champion?description.{champion}.name")
}

/// The champions listed: the ban/pick grid's, and any with positions of their own.
pub fn champions(cfg: &Config) -> Vec<String> {
    let mut all: BTreeSet<String> = lanes::known_names().into_iter().collect();
    all.extend(cfg.overrides.keys().cloned());
    all.into_iter().collect()
}

fn pages(count: usize) -> usize {
    count.div_ceil(PER_PAGE).max(1)
}

fn switch_on(cfg: &Config, key: &str) -> bool {
    match key {
        "enabled" => cfg.enabled,
        "ai" => cfg.ai,
        "player" => cfg.player,
        _ => cfg.history,
    }
}

/// The panel as `.ui` source, for `page` of `list`.
fn source(cfg: &Config, list: &[String], page: usize) -> String {
    let played = lanes::played();
    let mut body = String::new();
    body.push_str(&label("title", 28, 14, 800, 32, 22, TEXT, "Left", "智能位置锁定 · Smart Position Lock"));
    body.push_str(&label(
        "hint",
        28,
        44,
        1100,
        22,
        13,
        DIM,
        "Left",
        "点位置按钮设置英雄能打的位置，立即生效并保存到 settings.ini。F7 / Esc 关闭",
    ));
    // the game's own close button look
    body.push_str(
        "#close:button { width: 22px; height: 22px; anchor_x: 1; pivot_x: 1; x: -24px; y: 20px; \
         source: \"asset/base/ui/icons/cross\"; color: #c2c6ceff; hover: { color: #e8e8e8ff; } \
         active: { color: #e8e8e8ff; } } ",
    );
    for (i, (key, text)) in SWITCHES.iter().enumerate() {
        body.push_str(&button(&format!("sw_{key}"), 28 + i as i32 * 212, 80, 200, 36, switch_on(cfg, key), 14, text));
    }
    let count = pages(list.len());
    body.push_str(&button("prev", 1008, 80, 44, 36, false, 16, "◀"));
    body.push_str(&label("page", 1056, 80, 92, 36, 14, TEXT, "Center", &format!("{} / {count}", page + 1)));
    body.push_str(&button("next", 1152, 80, 44, 36, false, 16, "▶"));
    body.push_str(&label(
        "legend",
        28,
        124,
        1180,
        24,
        13,
        DIM,
        "Left",
        "绿色 = 能打这个位置 · 手动 = 你指定的位置（↺ 恢复自动） · 自动 = 选人卡片上的主位置 + 本存档实际打过的位置",
    ));
    for (i, champ) in list.iter().skip(page * PER_PAGE).take(PER_PAGE).enumerate() {
        let x = 28 + (i / PER_COLUMN) as i32 * 600;
        let y = 158 + (i % PER_COLUMN) as i32 * 54;
        let own = cfg.override_of(champ).is_some();
        let lit = lanes::allowed(champ, cfg, played.as_deref());
        let mut row = format!(
            "#face:color {{ x: 0px; y: 3px; width: 44px; height: 44px; color: {SLOT}; ignore_event: true; \
             rounding: Uniform {{ rounding: 8; }} #icon:image {{ width: 40px; height: 40px; anchor_x: 0.5; anchor_y: 0.5; \
             pivot_x: 0.5; pivot_y: 0.5; ignore_event: true; }} }} "
        );
        row.push_str(&label("name", 52, 0, 150, 50, 15, TEXT, "Left", &name_ref(champ)));
        for (lane, on) in lit.iter().enumerate() {
            row.push_str(&button(&format!("l{lane}"), 206 + lane as i32 * 58, 8, 54, 34, *on, 13, &position_ref(lane)));
        }
        row.push_str(&label("src", 500, 0, 40, 50, 12, if own { TEXT } else { DIM }, "Center", if own { "手动" } else { "自动" }));
        if own {
            row.push_str(&button("auto", 544, 8, 32, 34, false, 15, "↺"));
        }
        body.push_str(&format!(
            "#r{i}:empty {{ x: {x}px; y: {y}px; width: 580px; height: 50px; ignore_event: true; {row}}} "
        ));
    }
    body.push_str(&label(
        "foot",
        28,
        818,
        1180,
        24,
        13,
        DIM,
        "Left",
        &format!("{} 个英雄。英雄在选人界面出现过一次后才会列在这里。", list.len()),
    ));
    format!(
        "{NODE}:empty {{ width: 100%; height: 100%; ignore_event: true; \
         #shield:color_icon_button {{ anchor_x: 0.5; pivot_x: 0.5; anchor_y: 0.5; pivot_y: 0.5; width: {WIDTH}px; \
         height: {HEIGHT}px; btn: {{ color: #00000000; }} hover: {{ btn: {{ color: #00000000; }} }} }} \
         #box:color {{ anchor_x: 0.5; pivot_x: 0.5; anchor_y: 0.5; pivot_y: 0.5; width: {WIDTH}px; height: {HEIGHT}px; \
         color: {PANEL}; ignore_event: true; rounding: Uniform {{ rounding: 14; }} {body}}} }}"
    )
}

impl Panel {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// One frame. `screen` identifies the screen (another one closes the panel).
    pub fn tick(&mut self, ui: &mut impl Ui, frame: u64, screen: &str, keys: &[String], clicks: &[String]) {
        let parent = if ui.exists("main") { "main".to_string() } else { ui.children("").into_iter().next().unwrap_or_default() };
        let root = format!("{}.{NODE}", self.parent);
        let pressed = keys.iter().any(|k| k.eq_ignore_ascii_case(HOTKEY));
        let escape = keys.iter().any(|k| CLOSE_KEYS.iter().any(|c| k.eq_ignore_ascii_case(c)));
        let clicked = |what: &str| clicks.iter().any(|c| *c == format!("{root}.box.{what}"));
        if self.open && (pressed || escape || clicked("close") || screen != self.screen) {
            ui.remove(&root);
            self.open = false;
            self.drawn = None;
            return;
        }
        if !self.open {
            if !pressed || parent.is_empty() {
                return;
            }
            self.open = true;
            self.screen = screen.to_string();
            self.drawn = None;
        }

        // what was clicked on the drawn page
        let cfg = config::get();
        let list = champions(&cfg);
        self.page = self.page.min(pages(list.len()) - 1);
        if clicked("prev") {
            self.page = if self.page == 0 { pages(list.len()) - 1 } else { self.page - 1 };
        }
        if clicked("next") {
            self.page = (self.page + 1) % pages(list.len());
        }
        for (key, _) in SWITCHES {
            if clicked(&format!("sw_{key}")) {
                config::write_switch(key, !switch_on(&cfg, key));
            }
        }
        for (i, champ) in self.rows.iter().enumerate() {
            if clicked(&format!("r{i}.auto")) {
                config::write_positions(champ, None);
            }
            for lane in 0..5 {
                if clicked(&format!("r{i}.l{lane}")) {
                    let played = lanes::played();
                    let mut now: Lanes = lanes::allowed(champ, &cfg, played.as_deref());
                    now[lane] = !now[lane];
                    // a champion plays somewhere: the last position stays on
                    if now != [false; 5] {
                        config::write_positions(champ, Some(now));
                    }
                }
            }
        }

        // drawn again when anything shown changed, or the game rebuilt the screen under it
        let cfg = config::get();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.page, &list, lanes::generation(), std::sync::Arc::as_ptr(&cfg) as usize).hash(&mut h);
        let key = h.finish();
        let here = format!("{parent}.{NODE}");
        let missing = frame >= self.next_heal && !ui.exists(&here);
        if frame >= self.next_heal {
            self.next_heal = frame + HEAL_EVERY;
        }
        if self.drawn == Some(key) && !missing && parent == self.parent {
            return;
        }
        ui.remove(&root);
        ui.remove(&here);
        if !ui.spawn(&parent, &source(&cfg, &list, self.page)) {
            return;
        }
        self.parent = parent;
        self.drawn = Some(key);
        self.rows = list.iter().skip(self.page * PER_PAGE).take(PER_PAGE).cloned().collect();
        // the game keeps a node's handlers with the node: every new drawing registers its own
        let base = format!("{here}.box");
        for what in ["close", "prev", "next"] {
            ui.on_click(&format!("{base}.{what}"));
        }
        for (key, _) in SWITCHES {
            ui.on_click(&format!("{base}.sw_{key}"));
        }
        for (i, champ) in self.rows.iter().enumerate() {
            ui.set_champion_icon(&format!("{base}.r{i}.face.icon"), champ, 40.0);
            for lane in 0..5 {
                ui.on_click(&format!("{base}.r{i}.l{lane}"));
            }
            if cfg.override_of(champ).is_some() {
                ui.on_click(&format!("{base}.r{i}.auto"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::tests::{MID_SUP, TOP};
    use crate::ui::{take_clicks, tests::FakeUi};

    fn frame(p: &mut Panel, ui: &mut FakeUi, n: u64, keys: &[&str]) {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        let clicks = take_clicks();
        p.tick(ui, n, "Main", &keys, &clicks);
    }

    #[test]
    fn opens_sets_positions_and_switches_and_closes() {
        let _serial = crate::tests::serial();
        crate::tests::temp_dir();
        let _ = std::fs::remove_file(crate::paths::mod_dir().join(config::FILE));
        config::load_now();
        lanes::clear();
        for i in 0..30 {
            lanes::learn(&format!("league_c{i:02}"), TOP);
        }
        lanes::learn("league_c00", MID_SUP);
        let mut ui = FakeUi::default();
        ui.add("main", None);
        let mut p = Panel::default();
        frame(&mut p, &mut ui, 0, &[]);
        assert!(!ui.exists("main.spl_panel"), "closed until F7");
        frame(&mut p, &mut ui, 1, &["F7"]);
        let row = "main.spl_panel.box.r0";
        assert!(ui.exists(&format!("{row}.l4")) && ui.exists("main.spl_panel.box.r23.l0") && !ui.exists("main.spl_panel.box.r24"));
        assert_eq!(ui.icons.get(&format!("{row}.face.icon")).map(String::as_str), Some("league_c00"));
        assert_eq!(ui.text(&format!("{row}.name")).as_deref(), Some("#asset/base/text/champion?description.league_c00.name"));
        assert_eq!(ui.text(&format!("{row}.l2.t")).as_deref(), Some("#asset/base/text/ui?position.mid"));
        assert_eq!(ui.text(&format!("{row}.src")).as_deref(), Some("自动"));
        assert_eq!(ui.text("main.spl_panel.box.page").as_deref(), Some("1 / 2"));

        // a position clicked: the champion gets positions of its own, written to the file
        ui.click(&format!("{row}.l0"));
        frame(&mut p, &mut ui, 2, &[]);
        assert_eq!(config::get().override_of("league_c00"), Some([true, false, true, false, true]));
        assert_eq!(ui.text(&format!("{row}.src")).as_deref(), Some("手动"), "drawn again");
        let file = std::fs::read_to_string(crate::paths::mod_dir().join(config::FILE)).unwrap();
        assert!(file.contains("league_c00=Top,Mid,Support"), "{file}");
        // the last position stays on
        for lane in [0, 2] {
            ui.click(&format!("{row}.l{lane}"));
            frame(&mut p, &mut ui, 3 + lane as u64, &[]);
        }
        assert_eq!(config::get().override_of("league_c00"), Some([false, false, false, false, true]));
        ui.click(&format!("{row}.l4"));
        frame(&mut p, &mut ui, 8, &[]);
        assert_eq!(config::get().override_of("league_c00"), Some([false, false, false, false, true]), "not none");
        // back to working it out
        ui.click(&format!("{row}.auto"));
        frame(&mut p, &mut ui, 9, &[]);
        assert_eq!(config::get().override_of("league_c00"), None);
        assert!(!ui.exists(&format!("{row}.auto")));

        // a switch
        ui.click("main.spl_panel.box.sw_ai");
        frame(&mut p, &mut ui, 10, &[]);
        assert!(!config::get().ai);
        // the next page: the rest
        ui.click("main.spl_panel.box.next");
        frame(&mut p, &mut ui, 11, &[]);
        assert_eq!(ui.text("main.spl_panel.box.page").as_deref(), Some("2 / 2"));
        assert_eq!(ui.icons.get(&format!("{row}.face.icon")).map(String::as_str), Some("league_c24"));
        assert!(!ui.exists("main.spl_panel.box.r6"));

        // the game rebuilt the screen: drawn again; Esc closes; another screen closes
        ui.remove("main.spl_panel");
        frame(&mut p, &mut ui, 40, &[]);
        assert!(ui.exists("main.spl_panel.box.r0"));
        frame(&mut p, &mut ui, 41, &["Escape"]);
        assert!(!ui.exists("main.spl_panel") && !p.is_open());
        frame(&mut p, &mut ui, 42, &["F7"]);
        assert!(p.is_open());
        p.tick(&mut ui, 43, "Match", &[], &[]);
        assert!(!p.is_open() && !ui.exists("main.spl_panel"));
        // the close button
        frame(&mut p, &mut ui, 44, &["F7"]);
        ui.click("main.spl_panel.box.close");
        frame(&mut p, &mut ui, 45, &[]);
        assert!(!p.is_open());

        let _ = std::fs::remove_file(crate::paths::mod_dir().join(config::FILE));
        config::set(Config::default());
        lanes::clear();
    }
}
