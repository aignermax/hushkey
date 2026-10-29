//! Start at login: a LaunchAgent, `~/Library/LaunchAgents/com.hushkey.app.plist`,
//! pointing at the installed app (or this executable when none is
//! installed). No admin rights needed, removed by switching it off.
//!
//! The agent is only written, not loaded: launchd reads it at the next
//! login, and loading it now would start a second hushkey.

use std::path::PathBuf;

use super::setup;

const LABEL: &str = "com.hushkey.app";

fn plist_path() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| {
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        })
        .ok_or_else(|| "no home folder".to_string())
}

/// The program the agent starts.
fn program() -> Result<PathBuf, String> {
    let installed = setup::installed_exe();
    if installed.exists() {
        return Ok(installed);
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn plist(program: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>AssociatedBundleIdentifiers</key>
    <string>{LABEL}</string>
</dict>
</plist>
"#,
        program = xml_escape(program)
    )
}

/// Whether the agent starts this hushkey (one for another copy does not
/// count).
pub fn enabled() -> bool {
    let (Ok(path), Ok(program)) = (plist_path(), program()) else {
        return false;
    };
    std::fs::read_to_string(path).is_ok_and(|text| {
        text.contains(&format!(
            "<string>{}</string>",
            xml_escape(&program.to_string_lossy())
        ))
    })
}

pub fn set(on: bool) -> Result<(), String> {
    let path = plist_path()?;
    if on {
        let program = program()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, plist(&program.to_string_lossy()))
            .map_err(|e| format!("{}: {e}", path.display()))
    } else {
        // Not unloaded: if launchd started this very process, unloading
        // would end it. Without the file, the next login starts nothing.
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_starts_the_program_at_login() {
        let text = plist("/Users/a & b/Applications/hushkey.app/Contents/MacOS/hushkey");
        assert!(text.contains(
            "<string>/Users/a &amp; b/Applications/hushkey.app/Contents/MacOS/hushkey</string>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(text.contains("<string>com.hushkey.app</string>"));
    }
}
