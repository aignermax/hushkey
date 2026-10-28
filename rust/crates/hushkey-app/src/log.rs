//! A small log in the state folder. Like the Python app it never records
//! what was said, only that something happened and how long it was.

use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn open() {
    let dir = crate::config::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("hushkey.log"))
    {
        *FILE.lock().unwrap() = Some(file);
    }
}

pub fn line(message: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let text = format!("{secs} {message}\n");
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        let _ = file.write_all(text.as_bytes());
    }
    eprint!("{text}");
}
