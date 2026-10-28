//! Finding where utterances start and end in a stream of speech
//! probabilities, one per VAD frame.
//!
//! Hysteresis keeps a word's quiet syllables from splitting it: speech starts
//! above `threshold` and only counts as stopped below `threshold - 0.15`.

use crate::engine::VAD_FRAME;

#[derive(Clone, Debug)]
pub struct EndpointConfig {
    /// Probability above which a frame is speech.
    pub threshold: f32,
    /// Speech must last this many frames to start an utterance (filters
    /// clicks and breaths).
    pub min_speech_frames: usize,
    /// This many non-speech frames end an utterance.
    pub end_silence_frames: usize,
    /// Frames kept before the detected start (soft word onsets).
    pub pre_roll_frames: usize,
    /// Frames kept after the last speech frame.
    pub post_roll_frames: usize,
    /// An utterance is cut after this many frames even without a pause.
    pub max_utterance_frames: usize,
}

/// Frames in `ms` milliseconds, rounded to the nearest, at least one.
pub fn frames(ms: u32) -> usize {
    ((ms as usize * 16 + VAD_FRAME / 2) / VAD_FRAME).max(1)
}

impl Default for EndpointConfig {
    fn default() -> Self {
        EndpointConfig {
            threshold: 0.5,
            min_speech_frames: frames(200),
            end_silence_frames: frames(500),
            pre_roll_frames: frames(250),
            post_roll_frames: frames(150),
            max_utterance_frames: frames(30_000),
        }
    }
}

/// What a frame changed. Positions are absolute frame indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// An utterance began at `start` (pre-roll included).
    Started { start: usize },
    /// The utterance spans `[start, end)` frames.
    Ended { start: usize, end: usize },
}

#[derive(Clone, Debug)]
enum State {
    Silent,
    /// Speech seen since `first`, not yet long enough to count.
    Rising {
        first: usize,
        speech: usize,
    },
    Speaking {
        start: usize,
        last_speech: usize,
    },
}

pub struct Endpointer {
    config: EndpointConfig,
    state: State,
    next_frame: usize,
    /// End of the last utterance: the next one's pre-roll stops there, so no
    /// audio is transcribed twice (matters after a cut without a pause).
    floor: usize,
}

impl Endpointer {
    pub fn new(config: EndpointConfig) -> Self {
        Endpointer {
            config,
            state: State::Silent,
            next_frame: 0,
            floor: 0,
        }
    }

    /// Index of the next frame `push` expects.
    pub fn next_frame(&self) -> usize {
        self.next_frame
    }

    /// The start of the utterance in progress, if any.
    pub fn utterance_start(&self) -> Option<usize> {
        match self.state {
            State::Speaking { start, .. } => Some(start),
            _ => None,
        }
    }

    /// The earliest frame a later boundary can still refer to: audio before
    /// it may be dropped.
    pub fn keep_from(&self) -> usize {
        match self.state {
            State::Silent => self.next_frame.saturating_sub(self.config.pre_roll_frames),
            State::Rising { first, .. } => first.saturating_sub(self.config.pre_roll_frames),
            State::Speaking { start, .. } => start,
        }
    }

    /// Classify the next frame.
    pub fn push(&mut self, probability: f32) -> Option<Boundary> {
        let frame = self.next_frame;
        self.next_frame += 1;
        let c = &self.config;
        let speech = probability >= c.threshold;
        let still_speech = probability >= c.threshold - 0.15;
        match self.state.clone() {
            State::Silent => {
                if speech {
                    self.state = State::Rising {
                        first: frame,
                        speech: 1,
                    };
                    return self.maybe_start();
                }
                None
            }
            State::Rising {
                first,
                speech: count,
            } => {
                if still_speech {
                    self.state = State::Rising {
                        first,
                        speech: count + 1,
                    };
                    self.maybe_start()
                } else {
                    self.state = State::Silent;
                    None
                }
            }
            State::Speaking { start, last_speech } => {
                let last_speech = if still_speech { frame } else { last_speech };
                let silence = frame - last_speech;
                if silence >= c.end_silence_frames || frame + 1 - start >= c.max_utterance_frames {
                    self.state = State::Silent;
                    let end = (last_speech + 1 + c.post_roll_frames).min(frame + 1);
                    self.floor = end;
                    return Some(Boundary::Ended { start, end });
                }
                self.state = State::Speaking { start, last_speech };
                None
            }
        }
    }

    fn maybe_start(&mut self) -> Option<Boundary> {
        if let State::Rising { first, speech } = self.state {
            if speech >= self.config.min_speech_frames {
                let start = first
                    .saturating_sub(self.config.pre_roll_frames)
                    .max(self.floor);
                self.state = State::Speaking {
                    start,
                    last_speech: self.next_frame - 1,
                };
                return Some(Boundary::Started { start });
            }
        }
        None
    }

    /// End the utterance in progress at the current position (the input
    /// stopped).
    pub fn flush(&mut self) -> Option<Boundary> {
        let state = std::mem::replace(&mut self.state, State::Silent);
        match state {
            State::Speaking { start, last_speech } => {
                let end = (last_speech + 1 + self.config.post_roll_frames).min(self.next_frame);
                Some(Boundary::Ended { start, end })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EndpointConfig {
        EndpointConfig {
            threshold: 0.5,
            min_speech_frames: 3,
            end_silence_frames: 4,
            pre_roll_frames: 2,
            post_roll_frames: 1,
            max_utterance_frames: 100,
        }
    }

    fn run(probs: &[f32], config: EndpointConfig) -> Vec<Boundary> {
        let mut e = Endpointer::new(config);
        let mut out: Vec<Boundary> = probs.iter().filter_map(|&p| e.push(p)).collect();
        out.extend(e.flush());
        out
    }

    #[test]
    fn one_utterance_with_pre_and_post_roll() {
        // silence x5, speech x6 (frames 5..=10), silence x6
        let probs: Vec<f32> = [vec![0.0; 5], vec![0.9; 6], vec![0.0; 6]].concat();
        assert_eq!(
            run(&probs, config()),
            vec![
                Boundary::Started { start: 3 },
                Boundary::Ended { start: 3, end: 12 }
            ]
        );
    }

    #[test]
    fn a_click_shorter_than_min_speech_is_ignored() {
        let probs: Vec<f32> = [vec![0.0; 3], vec![0.9; 2], vec![0.0; 10]].concat();
        assert_eq!(run(&probs, config()), vec![]);
    }

    #[test]
    fn a_short_dip_does_not_split_a_word() {
        // speech, 2 frames of near-silence (< end_silence), speech, silence
        let probs: Vec<f32> = [vec![0.9; 4], vec![0.1; 2], vec![0.9; 4], vec![0.0; 6]].concat();
        assert_eq!(
            run(&probs, config()),
            vec![
                Boundary::Started { start: 0 },
                Boundary::Ended { start: 0, end: 11 }
            ]
        );
    }

    #[test]
    fn hysteresis_keeps_a_quiet_syllable_in_the_utterance() {
        // 0.4 is below threshold but above threshold - 0.15
        let probs: Vec<f32> = [vec![0.9; 4], vec![0.4; 6], vec![0.0; 5]].concat();
        let b = run(&probs, config());
        assert_eq!(b[1], Boundary::Ended { start: 0, end: 11 });
    }

    #[test]
    fn two_utterances_separated_by_a_pause() {
        let probs: Vec<f32> = [vec![0.9; 4], vec![0.0; 6], vec![0.9; 4], vec![0.0; 6]].concat();
        assert_eq!(
            run(&probs, config()),
            vec![
                Boundary::Started { start: 0 },
                Boundary::Ended { start: 0, end: 5 },
                Boundary::Started { start: 8 },
                Boundary::Ended { start: 8, end: 15 },
            ]
        );
    }

    #[test]
    fn endless_speech_is_cut_at_the_maximum() {
        let mut c = config();
        c.max_utterance_frames = 10;
        let b = run(&[0.9; 25], c);
        assert_eq!(b[0], Boundary::Started { start: 0 });
        assert_eq!(b[1], Boundary::Ended { start: 0, end: 10 });
    }

    #[test]
    fn after_a_cut_the_next_utterance_does_not_reach_back() {
        let mut c = config();
        c.max_utterance_frames = 10;
        let b = run(&[0.9; 25], c);
        assert_eq!(b[1], Boundary::Ended { start: 0, end: 10 });
        // Speech continues: the pre-roll would reach back to frame 9.
        assert_eq!(b[2], Boundary::Started { start: 10 });
        for pair in b.chunks(2).collect::<Vec<_>>().windows(2) {
            if let (Boundary::Ended { end, .. }, Boundary::Started { start }) =
                (pair[0][1], pair[1][0])
            {
                assert!(start >= end, "{b:?}");
            }
        }
    }

    #[test]
    fn flush_ends_an_utterance_in_progress() {
        let mut e = Endpointer::new(config());
        for _ in 0..5 {
            e.push(0.9);
        }
        assert_eq!(e.flush(), Some(Boundary::Ended { start: 0, end: 5 }));
        assert_eq!(e.flush(), None);
    }

    #[test]
    fn keep_from_never_passes_a_pending_start() {
        let mut e = Endpointer::new(config());
        for _ in 0..10 {
            e.push(0.0);
        }
        e.push(0.9); // rising at frame 10
        assert_eq!(e.keep_from(), 8);
        e.push(0.9);
        e.push(0.9); // speaking, start 8
        assert_eq!(e.utterance_start(), Some(8));
        for _ in 0..3 {
            e.push(0.0);
        }
        assert_eq!(e.keep_from(), 8);
    }
}
