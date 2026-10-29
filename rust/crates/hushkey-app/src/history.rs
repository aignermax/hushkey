//! The last few dictations, for the tray's "Recent dictations" menu: a
//! dictation that went into the wrong window (or nowhere) can be copied
//! again. A continuous-listening session is one entry that grows with
//! each utterance.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Entries kept, newest first (as in the Python app).
pub const SIZE: usize = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub text: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

pub struct History {
    path: PathBuf,
    entries: Vec<Entry>,
    /// The newest entry belongs to the listening session in progress and
    /// grows with each utterance.
    growing: bool,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl History {
    pub fn load(path: &Path) -> History {
        let entries = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<Entry>>(&text).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.text.trim().is_empty())
            .take(SIZE)
            .collect();
        History {
            path: path.to_path_buf(),
            entries,
            growing: false,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// A finished dictation.
    pub fn add(&mut self, text: &str) {
        self.growing = false;
        self.push(text);
        self.save();
    }

    /// Append an utterance to the listening session's entry; the first
    /// utterance of a session starts it.
    pub fn extend(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match self.entries.first_mut().filter(|_| self.growing) {
            Some(first) => {
                first.text.push(' ');
                first.text.push_str(text);
            }
            None => {
                self.push(text);
                self.growing = true;
            }
        }
        self.save();
    }

    /// End the listening session's entry; the next utterance starts anew.
    pub fn close(&mut self) {
        self.growing = false;
    }

    fn push(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.entries.insert(
            0,
            Entry {
                text: text.to_string(),
                at: now(),
            },
        );
        self.entries.truncate(SIZE);
    }

    /// Atomic: write a sibling file, then rename over the old one.
    fn save(&self) {
        let Ok(json) = serde_json::to_string_pretty(&self.entries) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let partial = self.path.with_extension("json.part");
        if std::fs::write(&partial, json).is_ok() {
            let _ = std::fs::rename(&partial, &self.path);
        }
    }
}

/// A menu label: the start of `text`, on one line.
pub fn label(text: &str) -> String {
    const MAX: usize = 48;
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX {
        line
    } else {
        line.chars().take(MAX - 1).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hk-history-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.json")
    }

    #[test]
    fn keeps_the_last_three_newest_first_across_restarts() {
        let path = temp();
        let mut h = History::load(&path);
        for t in ["eins", "zwei", "drei", "vier"] {
            h.add(t);
        }
        let texts: Vec<_> = History::load(&path)
            .entries()
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(texts, ["vier", "drei", "zwei"]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_listening_session_is_one_growing_entry() {
        let path = temp();
        let mut h = History::load(&path);
        h.add("vorher");
        h.extend("Erster Satz.");
        h.extend("  ");
        h.extend("Zweiter Satz.");
        h.close();
        h.extend("Neue Sitzung.");
        let texts: Vec<_> = History::load(&path)
            .entries()
            .iter()
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(
            texts,
            ["Neue Sitzung.", "Erster Satz. Zweiter Satz.", "vorher"]
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_broken_file_is_an_empty_history() {
        let path = temp();
        std::fs::write(&path, "{not json").unwrap();
        assert!(History::load(&path).entries().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn labels_are_one_short_line() {
        assert_eq!(label("Hallo\nWelt"), "Hallo Welt");
        let long = label(&"wort ".repeat(30));
        assert_eq!(long.chars().count(), 48);
        assert!(long.ends_with('…'));
    }
}
