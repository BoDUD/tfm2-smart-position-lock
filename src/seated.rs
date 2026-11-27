//! 屏幕上这一局（玩家自己的比赛）里锁位置给每支队排的分路，写进 Mod 文件夹的 `seated.json`（1.3.78）。
//!
//! 游戏把队伍的英雄按分路挪到各个选手的格子里，选人界面上的原画也跟着挪。Patch Meta AI（电竞大脑）在每个格子下面
//! 画位置和胜率，得知道挪完以后谁坐哪，它读这个文件。格式：
//! `{"seated": [{"team": 12, "picks": [按选人顺序的英雄], "slots": [每个分路格子的位置，0 上单 … 4 辅助],
//! "order": [每个格子坐第几手，picks 的下标]}]}`；锁位置没排（留给游戏）时 `order` 是 null。
//! 每次打开选人界面清空；别的比赛日在后台模拟的不记。

use std::sync::{Mutex, PoisonError};

use serde::Serialize;

pub const FILE: &str = "seated.json";
/// 一场两支队，留点余量
const KEPT: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub team: Option<usize>,
    pub picks: Vec<String>,
    pub slots: Vec<usize>,
    pub order: Option<Vec<usize>>,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// 新的一场：上一场的不要了
pub fn clear() {
    let mut list = ENTRIES.lock().unwrap_or_else(PoisonError::into_inner);
    list.clear();
    write(&list);
}

/// 记下一支队的分路，马上写进文件（同样的选人只留最新的）
pub fn record(entry: Entry) {
    let mut list = ENTRIES.lock().unwrap_or_else(PoisonError::into_inner);
    list.retain(|e| e.picks != entry.picks);
    list.push(entry);
    let extra = list.len().saturating_sub(KEPT);
    list.drain(..extra);
    write(&list);
}

/// 先写临时文件再换名，读的一方不会读到写了一半的
fn write(list: &[Entry]) {
    let dir = crate::paths::mod_dir();
    if !dir.is_dir() {
        return;
    }
    let text = serde_json::json!({ "seated": list }).to_string();
    let tmp = dir.join(format!("{FILE}.tmp"));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, dir.join(FILE)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_this_matchs_seatings() {
        let _serial = crate::tests::serial();
        let dir = std::env::temp_dir().join(format!("smart_position_lock_seated_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(crate::paths::DIR_ENV, &dir);
        let picks = |p: [&str; 5]| p.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        clear();
        record(Entry { team: Some(12), picks: picks(["a", "b", "c", "d", "e"]), slots: vec![0, 1, 2, 3, 4], order: Some(vec![1, 0, 2, 4, 3]) });
        record(Entry { team: Some(19), picks: picks(["f", "g", "h", "i", "j"]), slots: vec![0, 1, 2, 3, 4], order: None });
        // 同样的选人再排一次：只留新的
        record(Entry { team: Some(12), picks: picks(["a", "b", "c", "d", "e"]), slots: vec![0, 1, 2, 3, 4], order: Some(vec![0, 1, 2, 3, 4]) });
        let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(dir.join(FILE)).unwrap()).unwrap();
        let v = read();
        let list = v["seated"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["team"], 19);
        assert!(list[0]["order"].is_null(), "left to the game");
        assert_eq!(list[1]["picks"][0], "a");
        assert_eq!(list[1]["order"], serde_json::json!([0, 1, 2, 3, 4]));
        assert!(!dir.join(format!("{FILE}.tmp")).exists());
        clear();
        assert!(read()["seated"].as_array().unwrap().is_empty(), "a new match");
        std::env::remove_var(crate::paths::DIR_ENV);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
