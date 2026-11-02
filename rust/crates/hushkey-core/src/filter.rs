//! Text whisper produces from noise rather than speech.
//!
//! Given music, breathing or a door, whisper tends to write a bracketed
//! annotation ("[Musik]", "(applause)") or a line from the subtitles it was
//! trained on. Listening drops such utterances: they must not fire commands
//! or reach a brain.

/// Subtitle credits and sign-offs whisper is known to invent from silence
/// and noise, lowercased and without punctuation.
const PHANTOMS: &[&str] = &[
    "untertitel im auftrag des zdf",
    "untertitel im auftrag des zdf für funk",
    "untertitel der amara org community",
    "untertitelung des zdf",
    "vielen dank fürs zuschauen",
    "vielen dank für ihre aufmerksamkeit",
    "bis zum nächsten mal",
    "thank you for watching",
    "thanks for watching",
    "please subscribe",
    "subtitles by the amara org community",
];

fn words(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// True if `text` is no speech: empty, only bracketed or starred
/// annotations, or a known phantom line (a trailing year is ignored).
pub fn is_non_speech(text: &str) -> bool {
    let mut rest = String::new();
    let (mut brackets, mut starred) = (0u32, false);
    for c in text.chars() {
        match c {
            '[' | '(' => brackets += 1,
            ']' | ')' => brackets = brackets.saturating_sub(1),
            '*' => starred = !starred,
            _ if brackets == 0 && !starred => rest.push(c),
            _ => {}
        }
    }
    let mut rest: Vec<String> = words(&rest).split(' ').map(str::to_string).collect();
    let year = |w: &String| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit());
    while rest.len() > 1 && rest.last().is_some_and(year) {
        rest.pop(); // "Untertitel im Auftrag des ZDF, 2021"
    }
    let rest = rest.join(" ");
    rest.is_empty() || PHANTOMS.contains(&rest.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_and_phantoms_are_no_speech() {
        for text in [
            "",
            "  ",
            "[Musik]",
            "(Applaus)",
            "*Seufzt*",
            "[MUSIC] [MUSIC]",
            "Untertitel im Auftrag des ZDF, 2021",
            "Untertitel im Auftrag des ZDF für funk, 2017",
            "Vielen Dank fürs Zuschauen!",
            "Thank you for watching.",
            "...",
        ] {
            assert!(is_non_speech(text), "{text:?}");
        }
    }

    #[test]
    fn real_utterances_are_speech() {
        for text in [
            "Weiter.",
            "Kanal 6",
            "Vielen Dank.",
            "Wie spät ist es eigentlich?",
            "Notiz (wichtig): Pin drei wackelt",
            "Thank you",
            "6",
            "2024",
        ] {
            assert!(!is_non_speech(text), "{text:?}");
        }
    }
}
