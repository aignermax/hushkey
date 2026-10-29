//! Voice commands: utterances matched against phrases with slots.
//!
//! A command set is JSON:
//!
//! ```json
//! {"threshold": 0.85,
//!  "commands": [
//!    {"name": "next",    "phrases": ["weiter", "next"]},
//!    {"name": "channel", "phrases": ["kanal {n}", "channel {n}"]},
//!    {"name": "note",    "phrases": ["notiz {text}"]}]}
//! ```
//!
//! `{n}` is a whole number, spoken as digits ("6") or as German or English
//! words ("sechs", "einundzwanzig", "twenty one"); `{word}` is one word;
//! `{text}` is one or more words. A phrase must cover the whole utterance.
//! Words match despite small recognition slips ("kanall"), but a doubtful
//! match yields no command: a command that fires on something nobody said
//! is worse than one that has to be repeated.

use serde::Deserialize;

use crate::error::{Error, Result};

/// A recognized command.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandMatch {
    pub name: String,
    /// Slot values in phrase order; numbers as decimal digits.
    pub args: Vec<String>,
    /// The phrase that matched, as written in the command set.
    pub phrase: String,
    /// How closely the words matched, 0..1 (1 = exactly).
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Word(String),
    Number,
    OneWord,
    Text,
}

#[derive(Clone, Debug)]
struct Phrase {
    source: String,
    parts: Vec<Part>,
}

#[derive(Clone, Debug)]
struct Command {
    name: String,
    phrases: Vec<Phrase>,
}

#[derive(Clone, Debug)]
pub struct CommandSet {
    commands: Vec<Command>,
    threshold: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetJson {
    #[serde(default)]
    threshold: Option<f32>,
    commands: Vec<CommandJson>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandJson {
    name: String,
    phrases: Vec<String>,
}

/// Lowest similarity a single word may have: below it the words differ,
/// whatever the average says.
const WORD_FLOOR: f32 = 0.7;

impl CommandSet {
    /// Parse a command set (see the module docs for the format).
    pub fn from_json(json: &str) -> Result<CommandSet> {
        let set: SetJson = serde_json::from_str(json)
            .map_err(|e| Error::InvalidArgument(format!("command set: {e}")))?;
        let threshold = set.threshold.unwrap_or(0.85);
        if !(0.0..=1.0).contains(&threshold) {
            return Err(Error::InvalidArgument(format!(
                "command set: threshold must be between 0 and 1, not {threshold}"
            )));
        }
        let mut commands = Vec::new();
        for c in set.commands {
            if c.name.trim().is_empty() {
                return Err(Error::InvalidArgument(
                    "command set: a command has no name".into(),
                ));
            }
            if c.phrases.is_empty() {
                return Err(Error::InvalidArgument(format!(
                    "command set: {} has no phrases",
                    c.name
                )));
            }
            let phrases = c
                .phrases
                .iter()
                .map(|p| {
                    parse_phrase(p).map_err(|e| {
                        Error::InvalidArgument(format!("command set: {}: {e}", c.name))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            commands.push(Command {
                name: c.name,
                phrases,
            });
        }
        Ok(CommandSet {
            commands,
            threshold,
        })
    }

    /// The command words, for whisper's prompt: short words are recognized
    /// far more reliably when whisper expects them.
    pub fn prompt(&self) -> String {
        let mut words: Vec<&str> = Vec::new();
        for command in &self.commands {
            for phrase in &command.phrases {
                for part in &phrase.parts {
                    if let Part::Word(w) = part {
                        if !words.contains(&w.as_str()) {
                            words.push(w);
                        }
                    }
                }
            }
        }
        words.join(", ")
    }

    /// The best command `text` is, if any is close enough.
    pub fn match_text(&self, text: &str) -> Option<CommandMatch> {
        let tokens = normalize(text);
        if tokens.is_empty() {
            return None;
        }
        let mut best: Option<(f32, usize, CommandMatch)> = None;
        for command in &self.commands {
            for phrase in &command.phrases {
                let Some((confidence, args)) = match_phrase(&phrase.parts, &tokens) else {
                    continue;
                };
                if confidence < self.threshold {
                    continue;
                }
                let words = phrase
                    .parts
                    .iter()
                    .filter(|p| matches!(p, Part::Word(_)))
                    .count();
                // Higher confidence wins; on a tie the more specific phrase.
                let better = match &best {
                    None => true,
                    Some((c, w, _)) => {
                        confidence > *c + 1e-6 || ((confidence - c).abs() <= 1e-6 && words > *w)
                    }
                };
                if better {
                    best = Some((
                        confidence,
                        words,
                        CommandMatch {
                            name: command.name.clone(),
                            args,
                            phrase: phrase.source.clone(),
                            confidence,
                        },
                    ));
                }
            }
        }
        best.map(|b| b.2)
    }
}

fn parse_phrase(source: &str) -> std::result::Result<Phrase, String> {
    let mut parts = Vec::new();
    for raw in source.split_whitespace() {
        let part = match raw {
            "{n}" => Part::Number,
            "{word}" => Part::OneWord,
            "{text}" => Part::Text,
            _ if raw.contains('{') || raw.contains('}') => {
                return Err(format!(
                    "unknown slot {raw:?} in {source:?}; use {{n}}, {{word}} or {{text}}"
                ))
            }
            _ => {
                let words = normalize(raw);
                if words.is_empty() {
                    continue; // punctuation only
                }
                for w in words {
                    parts.push(Part::Word(w));
                }
                continue;
            }
        };
        parts.push(part);
    }
    if !parts.iter().any(|p| matches!(p, Part::Word(_))) {
        return Err(format!("{source:?} has no words; it would match anything"));
    }
    Ok(Phrase {
        source: source.to_string(),
        parts,
    })
}

/// Lowercase words; punctuation and symbols become separators.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Match all of `tokens` against `parts`. Returns the mean similarity of the
/// literal words and numbers, and the slot values.
fn match_phrase(parts: &[Part], tokens: &[String]) -> Option<(f32, Vec<String>)> {
    let mut best: Option<(f32, f32, Vec<String>)> = None; // (sum, count, args)
    search(parts, tokens, 0.0, 0.0, &mut Vec::new(), &mut best);
    best.map(|(sum, count, args)| (sum / count.max(1.0), args))
}

fn search(
    parts: &[Part],
    tokens: &[String],
    sum: f32,
    count: f32,
    args: &mut Vec<String>,
    best: &mut Option<(f32, f32, Vec<String>)>,
) {
    let Some((part, rest)) = parts.split_first() else {
        if tokens.is_empty() {
            let better = match best {
                None => true,
                Some((s, c, _)) => sum / count.max(1.0) > *s / c.max(1.0),
            };
            if better {
                *best = Some((sum, count, args.clone()));
            }
        }
        return;
    };
    match part {
        Part::Word(word) => {
            let Some((token, tail)) = tokens.split_first() else {
                return;
            };
            let similarity = similarity(word, token);
            if similarity >= WORD_FLOOR {
                search(rest, tail, sum + similarity, count + 1.0, args, best);
            }
        }
        Part::OneWord => {
            let Some((token, tail)) = tokens.split_first() else {
                return;
            };
            args.push(token.clone());
            search(rest, tail, sum, count, args, best);
            args.pop();
        }
        Part::Number => {
            // A number where the phrase expects one is evidence like an
            // exact word: "kanall 7" is a command, a lone "wetter" is not
            // "weiter", though both are one letter off.
            for (value, used) in parse_numbers(tokens) {
                args.push(value.to_string());
                search(rest, &tokens[used..], sum + 1.0, count + 1.0, args, best);
                args.pop();
            }
        }
        Part::Text => {
            for used in 1..=tokens.len() {
                args.push(tokens[..used].join(" "));
                search(rest, &tokens[used..], sum, count, args, best);
                args.pop();
            }
        }
    }
}

/// 1 - edit distance / longer length, over characters.
fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous + usize::from(ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    1.0 - row[b.len()] as f32 / longest as f32
}

/// Every way the start of `tokens` reads as a number: (value, tokens used).
fn parse_numbers(tokens: &[String]) -> Vec<(u64, usize)> {
    let mut out = Vec::new();
    if let Some(first) = tokens.first() {
        if let Ok(n) = first.parse::<u64>() {
            out.push((n, 1));
        } else if let Some(n) = german_number(first) {
            out.push((n, 1));
        }
    }
    // "one hundred and twenty one" is five words.
    for used in 1..=tokens.len().min(6) {
        if let Some(n) = english_number(&tokens[..used]) {
            out.push((n, used));
        }
    }
    out
}

const DE_UNITS: [&str; 20] = [
    "null",
    "eins",
    "zwei",
    "drei",
    "vier",
    "fünf",
    "sechs",
    "sieben",
    "acht",
    "neun",
    "zehn",
    "elf",
    "zwölf",
    "dreizehn",
    "vierzehn",
    "fünfzehn",
    "sechzehn",
    "siebzehn",
    "achtzehn",
    "neunzehn",
];
const DE_TENS: [&str; 8] = [
    "zwanzig", "dreißig", "vierzig", "fünfzig", "sechzig", "siebzig", "achtzig", "neunzig",
];

/// German number words 0..999 as whisper writes them: "sechs",
/// "einundzwanzig", "dreihundertzwölf", "hundert".
fn german_number(word: &str) -> Option<u64> {
    let word = word.replace("dreissig", "dreißig");
    if let Some(pos) = word.find("hundert") {
        let (left, right) = (&word[..pos], &word[pos + "hundert".len()..]);
        let hundreds = match left {
            "" | "ein" | "eins" => 1,
            _ => german_below_100(left).filter(|n| (1..10).contains(n))?,
        };
        let rest = if right.is_empty() {
            0
        } else {
            german_below_100(right.strip_prefix("und").unwrap_or(right))?
        };
        return Some(hundreds * 100 + rest);
    }
    german_below_100(&word)
}

fn german_below_100(word: &str) -> Option<u64> {
    let unit = |w: &str| match w {
        "ein" | "eine" | "einen" => Some(1),
        _ => DE_UNITS.iter().position(|u| *u == w).map(|n| n as u64),
    };
    if let Some(n) = unit(word) {
        return Some(n);
    }
    if let Some(i) = DE_TENS.iter().position(|t| *t == word) {
        return Some(20 + 10 * i as u64);
    }
    // "einundzwanzig": units, "und", tens
    for (i, tens) in DE_TENS.iter().enumerate() {
        if let Some(left) = word.strip_suffix(tens).and_then(|l| l.strip_suffix("und")) {
            let u = unit(left).filter(|u| (1..10).contains(u))?;
            return Some(20 + 10 * i as u64 + u);
        }
    }
    None
}

const EN_UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const EN_TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// English number words 0..999 over exactly these tokens: "six",
/// "twenty one", "one hundred and five", "a hundred".
fn english_number(tokens: &[String]) -> Option<u64> {
    let words: Vec<&str> = tokens.iter().map(String::as_str).collect();
    match words.as_slice() {
        [hundreds, "hundred", rest @ ..] => {
            let h = match *hundreds {
                "a" | "one" => 1,
                w => EN_UNITS
                    .iter()
                    .position(|u| *u == w)
                    .filter(|n| (1..10).contains(n))? as u64,
            };
            let rest = match rest {
                [] => 0,
                ["and", rest @ ..] | rest => english_below_100(rest)?,
            };
            Some(h * 100 + rest)
        }
        ["hundred", rest @ ..] => {
            let rest = match rest {
                [] => 0,
                ["and", rest @ ..] | rest => english_below_100(rest)?,
            };
            Some(100 + rest)
        }
        rest => english_below_100(rest),
    }
}

fn english_below_100(words: &[&str]) -> Option<u64> {
    let unit = |w: &str| EN_UNITS.iter().position(|u| *u == w).map(|n| n as u64);
    let tens = |w: &str| {
        EN_TENS
            .iter()
            .position(|t| *t == w)
            .map(|i| 20 + 10 * i as u64)
    };
    match words {
        [w] => unit(w).or_else(|| tens(w)),
        [t, u] => {
            let u = unit(u).filter(|u| (1..10).contains(u))?;
            Some(tens(t)? + u)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab() -> CommandSet {
        CommandSet::from_json(
            r#"{"commands": [
                {"name": "next", "phrases": ["weiter", "next", "nächster kanal"]},
                {"name": "back", "phrases": ["zurück", "back"]},
                {"name": "channel", "phrases": ["kanal {n}", "channel {n}"]},
                {"name": "note", "phrases": ["notiz {text}"]},
                {"name": "pin", "phrases": ["pin {word} auf {n}"]}
            ]}"#,
        )
        .unwrap()
    }

    fn matched(text: &str) -> Option<(String, Vec<String>)> {
        lab().match_text(text).map(|m| (m.name, m.args))
    }

    fn cmd(name: &str, args: &[&str]) -> Option<(String, Vec<String>)> {
        Some((
            name.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ))
    }

    #[test]
    fn plain_words_as_whisper_writes_them() {
        assert_eq!(matched("Weiter."), cmd("next", &[]));
        assert_eq!(matched("weiter!"), cmd("next", &[]));
        assert_eq!(matched("Next"), cmd("next", &[]));
        assert_eq!(matched("Zurück."), cmd("back", &[]));
        assert_eq!(matched("Nächster Kanal."), cmd("next", &[]));
    }

    #[test]
    fn numbers_as_digits_and_words() {
        assert_eq!(matched("Kanal 6."), cmd("channel", &["6"]));
        assert_eq!(matched("Kanal sechs"), cmd("channel", &["6"]));
        assert_eq!(matched("Kanal einundzwanzig."), cmd("channel", &["21"]));
        assert_eq!(matched("Kanal dreihundertzwölf"), cmd("channel", &["312"]));
        assert_eq!(matched("Kanal hundert"), cmd("channel", &["100"]));
        assert_eq!(matched("Channel twenty one."), cmd("channel", &["21"]));
        assert_eq!(
            matched("channel one hundred and five"),
            cmd("channel", &["105"])
        );
        assert_eq!(
            matched("Channel one hundred and twenty one."),
            cmd("channel", &["121"])
        );
        assert_eq!(matched("Channel 42"), cmd("channel", &["42"]));
    }

    #[test]
    fn small_recognition_slips_still_match() {
        assert_eq!(matched("Kanall 7"), cmd("channel", &["7"]));
        assert_eq!(matched("weiterr"), cmd("next", &[]));
    }

    #[test]
    fn doubtful_or_partial_utterances_are_no_command() {
        assert_eq!(matched("Wetter"), None);
        assert_eq!(matched("weiter so, das war gut"), None);
        assert_eq!(matched("Kanal"), None);
        assert_eq!(matched("Kanal sieben acht neun zehn"), None);
        assert_eq!(matched(""), None);
        assert_eq!(matched("..."), None);
        assert_eq!(matched("Ich gehe zurück nach Hause"), None);
    }

    #[test]
    fn text_and_word_slots() {
        assert_eq!(
            matched("Notiz: Pin drei wackelt."),
            cmd("note", &["pin drei wackelt"])
        );
        assert_eq!(matched("Pin A auf 5"), cmd("pin", &["a", "5"]));
        assert_eq!(matched("Notiz"), None);
    }

    #[test]
    fn confidence_and_phrase_are_reported() {
        let exact = lab().match_text("Kanal 3").unwrap();
        assert_eq!(exact.confidence, 1.0);
        assert_eq!(exact.phrase, "kanal {n}");
        let slip = lab().match_text("kanall 3").unwrap();
        assert!(slip.confidence < 1.0 && slip.confidence > 0.8);
    }

    #[test]
    fn the_prompt_lists_every_command_word_once() {
        assert_eq!(
            lab().prompt(),
            "weiter, next, nächster, kanal, zurück, back, channel, notiz, pin, auf"
        );
    }

    #[test]
    fn invalid_command_sets_are_rejected_with_a_reason() {
        for (json, reason) in [
            ("{", "command set"),
            (
                r#"{"commands": [{"name": "x", "phrases": []}]}"#,
                "no phrases",
            ),
            (
                r#"{"commands": [{"name": "x", "phrases": ["{n}"]}]}"#,
                "no words",
            ),
            (
                r#"{"commands": [{"name": "x", "phrases": ["kanal {zahl}"]}]}"#,
                "unknown slot",
            ),
            (r#"{"threshold": 2, "commands": []}"#, "threshold"),
            (
                r#"{"commands": [{"name": "", "phrases": ["a"]}]}"#,
                "no name",
            ),
        ] {
            let err = CommandSet::from_json(json).unwrap_err().to_string();
            assert!(err.contains(reason), "{json}: {err}");
        }
    }

    #[test]
    fn german_numbers() {
        for (w, n) in [
            ("null", 0),
            ("eins", 1),
            ("zwölf", 12),
            ("zwanzig", 20),
            ("dreiunddreißig", 33),
            ("dreiunddreissig", 33),
            ("neunundneunzig", 99),
            ("einhundert", 100),
            ("hundertfünf", 105),
            ("zweihundertvierzig", 240),
            ("neunhundertneunundneunzig", 999),
        ] {
            assert_eq!(german_number(w), Some(n), "{w}");
        }
        for w in ["zwanzigund", "einhundertx", "kanal", "elfundzwanzig"] {
            assert_eq!(german_number(w), None, "{w}");
        }
    }
}
