//! The UI calls the mod needs (the game's client context, or a test double), and the clicks on
//! the mod's own buttons.
//!
//! UI paths are dot-separated node ids from the root of the loaded layout (the ban/pick screen
//! is `main`). The game rebuilds its screens on its own, so nothing assumes a node exists without
//! asking.

use std::sync::{Mutex, PoisonError};

use mod_api_stable::{InputEventKindV1, StableClient, UiEventKindV1};

pub trait Ui {
    fn exists(&self, path: &str) -> bool;
    fn children(&self, path: &str) -> Vec<String>;
    fn text(&self, path: &str) -> Option<String>;
    fn visible(&self, path: &str) -> Option<bool>;
    fn spawn(&mut self, parent: &str, source: &str) -> bool;
    fn set_visible(&mut self, path: &str, visible: bool) -> bool;
    fn set_text(&mut self, path: &str, text: &str) -> bool;
    fn set_properties(&mut self, path: &str, source: &str) -> bool;
    fn remove(&mut self, path: &str) -> bool;
    /// Keys pressed this frame (engine key names, e.g. "F7").
    fn keys_pressed(&self) -> Vec<String>;
    /// Clicks on `path` are queued for [`take_clicks`].
    fn on_click(&mut self, path: &str) -> bool;
    /// Fills an image node with a champion's face.
    fn set_champion_icon(&mut self, path: &str, champion: &str, size: f32) -> bool;
}

static CLICKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The paths clicked since the last call, each once (the game may deliver a click twice).
pub fn take_clicks() -> Vec<String> {
    let mut list = std::mem::take(&mut *CLICKS.lock().unwrap_or_else(PoisonError::into_inner));
    let mut seen = std::collections::HashSet::new();
    list.retain(|p| seen.insert(p.clone()));
    list
}

fn push_click(path: String) {
    CLICKS.lock().unwrap_or_else(PoisonError::into_inner).push(path);
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
    fn set_text(&mut self, path: &str, text: &str) -> bool {
        self.ui_set_text(path, text)
    }
    fn set_properties(&mut self, path: &str, source: &str) -> bool {
        self.ui_set_properties(path, source)
    }
    fn remove(&mut self, path: &str) -> bool {
        self.ui_remove_node(path)
    }
    fn keys_pressed(&self) -> Vec<String> {
        self.input_events()
            .into_iter()
            .filter(|e| e.kind == Some(InputEventKindV1::KeyPressed))
            .map(|e| e.key)
            .collect()
    }
    fn on_click(&mut self, path: &str) -> bool {
        let owned = path.to_string();
        self.ui_register_path_events(path, move |ctx| {
            // a click (or a host that does not say which event); not hovers or node removal
            if matches!(ctx.ui_current_event().and_then(|e| e.kind), None | Some(UiEventKindV1::Click)) {
                push_click(owned.clone());
            }
        })
    }
    fn set_champion_icon(&mut self, path: &str, champion: &str, size: f32) -> bool {
        self.ui_set_champion_icon(path, champion, size, size, 2.0)
    }
}

/// Text for a `.ui` string literal.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A UI tree in memory: nodes by path with text and visibility, every spawn, property
    /// write, click handler and champion icon.
    #[derive(Default)]
    pub struct FakeUi {
        pub nodes: BTreeMap<String, (Option<String>, bool)>,
        pub spawns: usize,
        pub sources: Vec<String>,
        pub props: BTreeMap<String, Vec<String>>,
        pub handlers: Vec<String>,
        pub icons: BTreeMap<String, String>,
        pub keys: Vec<String>,
    }

    impl FakeUi {
        pub fn add(&mut self, path: &str, text: Option<&str>) {
            self.nodes.insert(path.to_string(), (text.map(str::to_string), true));
        }
        pub fn hide(&mut self, path: &str) {
            if let Some(n) = self.nodes.get_mut(path) {
                n.1 = false;
            }
        }
        /// The player clicks a node: every handler registered on it fires.
        pub fn click(&self, path: &str) {
            for p in &self.handlers {
                if p == path && self.nodes.contains_key(path) {
                    push_click(p.clone());
                }
            }
        }
    }

    /// The `#id` of every node in `.ui` source, with its parent path; `text:` of labels.
    fn nodes_of(source: &str, parent: &str) -> Vec<(String, Option<String>, bool)> {
        let mut out: Vec<(String, Option<String>, bool)> = Vec::new();
        let mut stack: Vec<String> = vec![parent.to_string()];
        let mut chars = source.chars().peekable();
        let mut token = String::new();
        let mut pending: Option<String> = None;
        let mut first = true;
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    match pending.take() {
                        Some(n) => {
                            let path = format!("{}.{}", stack.last().cloned().unwrap_or_default(), n);
                            out.push((path.clone(), None, true));
                            stack.push(path);
                        }
                        None => stack.push(stack.last().cloned().unwrap_or_default()),
                    }
                    token.clear();
                }
                '}' => {
                    stack.pop();
                    token.clear();
                }
                '"' => {
                    let mut lit = String::new();
                    while let Some(d) = chars.next() {
                        match d {
                            '\\' => {
                                if let Some(e) = chars.next() {
                                    lit.push(e);
                                }
                            }
                            '"' => break,
                            d => lit.push(d),
                        }
                    }
                    // `text: "..."` of the node being defined
                    if token.trim().trim_end_matches(':').trim() == "text" {
                        let me = stack.last().cloned().unwrap_or_default();
                        if let Some(n) = out.iter_mut().rev().find(|n| n.0 == me) {
                            n.1 = Some(lit);
                        }
                    }
                    token.clear();
                }
                ';' => {
                    let t = token.replace(' ', "");
                    if t == "visible:false" {
                        let me = stack.last().cloned().unwrap_or_default();
                        if let Some(n) = out.iter_mut().rev().find(|n| n.0 == me) {
                            n.2 = false;
                        }
                    }
                    token.clear();
                }
                c if c.is_whitespace() => token.push(' '),
                c => {
                    token.push(c);
                    if let Some(':') = chars.peek() {
                        let name = token.trim().trim_start_matches('#').to_string();
                        if token.trim().starts_with('#') || first {
                            pending = Some(name);
                        }
                        first = false;
                    }
                }
            }
        }
        out
    }

    impl Ui for FakeUi {
        fn exists(&self, path: &str) -> bool {
            self.nodes.contains_key(path)
        }
        fn children(&self, path: &str) -> Vec<String> {
            let prefix = if path.is_empty() { String::new() } else { format!("{path}.") };
            self.nodes.keys().filter_map(|k| k.strip_prefix(&prefix)).filter(|r| !r.is_empty() && !r.contains('.')).map(str::to_string).collect()
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
            for (path, text, visible) in nodes_of(source, parent) {
                self.nodes.insert(path, (text, visible));
            }
            self.spawns += 1;
            self.sources.push(source.to_string());
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
        fn set_text(&mut self, path: &str, text: &str) -> bool {
            match self.nodes.get_mut(path) {
                Some(n) => {
                    n.0 = Some(text.to_string());
                    true
                }
                None => false,
            }
        }
        fn set_properties(&mut self, path: &str, source: &str) -> bool {
            if !self.nodes.contains_key(path) {
                return false;
            }
            self.props.entry(path.to_string()).or_default().push(source.to_string());
            true
        }
        fn remove(&mut self, path: &str) -> bool {
            let prefix = format!("{path}.");
            let before = self.nodes.len();
            self.nodes.retain(|k, _| k != path && !k.starts_with(&prefix));
            before != self.nodes.len()
        }
        fn keys_pressed(&self) -> Vec<String> {
            self.keys.clone()
        }
        fn on_click(&mut self, path: &str) -> bool {
            self.handlers.push(path.to_string());
            true
        }
        fn set_champion_icon(&mut self, path: &str, champion: &str, size: f32) -> bool {
            if !self.nodes.contains_key(path) || size <= 0.0 {
                return false;
            }
            self.icons.insert(path.to_string(), champion.to_string());
            true
        }
    }

    #[test]
    fn spawned_source_creates_its_nodes() {
        let mut ui = FakeUi::default();
        ui.add("main", None);
        assert!(ui.spawn(
            "main",
            r#"p:empty { #bar:color { visible: false; #text:label { text: "a{b \"c\""; } } #tag:label { } }"#
        ));
        assert!(ui.exists("main.p.bar.text") && ui.exists("main.p.tag"));
        assert_eq!(ui.text("main.p.bar.text").as_deref(), Some("a{b \"c\""));
        assert_eq!(ui.visible("main.p.bar"), Some(false));
        assert_eq!(ui.children("main.p"), ["bar", "tag"]);
        assert!(ui.remove("main.p") && !ui.exists("main.p.bar"));
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }
}
