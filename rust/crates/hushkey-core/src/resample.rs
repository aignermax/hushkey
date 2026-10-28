//! Streaming conversion of any input rate to whisper's 16 kHz.
//!
//! Windowed-sinc interpolation evaluated at each output instant: one filter
//! both band-limits (below the lower Nyquist rate, so 48 kHz input does not
//! alias into speech) and interpolates the fractional position. Microphones
//! deliver blocks of arbitrary size, so the filter history carries across
//! calls and the output is identical however the input was split.

const HALF_TAPS: usize = 16; // per side, in units of the slower rate
const ROLLOFF: f64 = 0.92; // cutoff as a fraction of the lower Nyquist rate

pub struct Resampler {
    step: f64,         // input samples per output sample
    cutoff: f64,       // cycles per input sample
    half_width: usize, // filter half-length in input samples
    history: Vec<f32>, // input samples not yet consumed by the filter
    position: f64,     // next output instant, relative to history[0]
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        assert!(
            input_rate > 0 && output_rate > 0,
            "sample rates must be positive"
        );
        let step = input_rate as f64 / output_rate as f64;
        let cutoff = 0.5 * ROLLOFF * (output_rate as f64 / input_rate as f64).min(1.0);
        let half_width = (HALF_TAPS as f64 * step.max(1.0)).ceil() as usize;
        Resampler {
            step,
            cutoff,
            half_width,
            // Leading silence lets the first output sample sit at time zero.
            history: vec![0.0; half_width],
            position: half_width as f64,
        }
    }

    /// Feed input samples; returns every output sample now computable.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.history.extend_from_slice(input);
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        while self.position + self.half_width as f64 + 1.0 < self.history.len() as f64 {
            out.push(self.sample_at(self.position));
            self.position += self.step;
        }
        // Drop history the filter can no longer reach.
        let keep_from = (self.position as usize).saturating_sub(self.half_width + 1);
        if keep_from > 0 {
            self.history.drain(..keep_from);
            self.position -= keep_from as f64;
        }
        out
    }

    fn sample_at(&self, t: f64) -> f32 {
        let center = t.floor() as isize;
        let lo = (center - self.half_width as isize).max(0) as usize;
        let hi = ((center + self.half_width as isize + 1) as usize).min(self.history.len());
        let mut acc = 0.0f64;
        for (i, &x) in self.history[lo..hi].iter().enumerate() {
            let d = (lo + i) as f64 - t;
            acc += x as f64 * self.kernel(d);
        }
        acc as f32
    }

    fn kernel(&self, d: f64) -> f64 {
        let w = self.half_width as f64;
        if d.abs() >= w {
            return 0.0;
        }
        let x = 2.0 * self.cutoff * d;
        let sinc = if x.abs() < 1e-9 {
            1.0
        } else {
            (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
        };
        // Blackman window over [-w, w].
        let n = (d + w) / (2.0 * w);
        let tau = 2.0 * std::f64::consts::PI;
        let window = 0.42 - 0.5 * (tau * n).cos() + 0.08 * (2.0 * tau * n).cos();
        2.0 * self.cutoff * sinc * window
    }
}

/// Resample a whole buffer in one go.
pub fn resample(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input_rate == output_rate {
        return input.to_vec();
    }
    let mut resampler = Resampler::new(input_rate, output_rate);
    let mut out = resampler.process(input);
    // Flush the filter tail with silence so the end of speech is kept.
    out.extend(resampler.process(&vec![0.0; resampler.half_width * 2 + 2]));
    let expected = (input.len() as u64 * output_rate as u64 / input_rate as u64) as usize;
    out.truncate(expected);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let n = (rate as f64 * seconds) as usize;
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn speech_band_tone_passes_at_the_right_length() {
        let out = resample(&tone(1000.0, 48_000, 1.0), 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        // 0.5 amplitude sine: RMS 0.354, kept within a few percent.
        let r = rms(&out[1000..15_000]);
        assert!((r - 0.3536).abs() < 0.02, "rms {r}");
    }

    #[test]
    fn above_nyquist_tone_does_not_alias_into_speech() {
        // 12 kHz would fold to 4 kHz without the low-pass.
        let out = resample(&tone(12_000.0, 48_000, 1.0), 48_000, 16_000);
        assert!(rms(&out[1000..15_000]) < 0.01);
    }

    #[test]
    fn streaming_in_odd_blocks_matches_one_shot() {
        let input = tone(440.0, 44_100, 0.5);
        let mut streaming = Resampler::new(44_100, 16_000);
        let mut out = Vec::new();
        for block in input.chunks(333) {
            out.extend(streaming.process(block));
        }
        let mut one_shot = Resampler::new(44_100, 16_000);
        let reference = one_shot.process(&input);
        assert_eq!(out.len(), reference.len());
        for (a, b) in out.iter().zip(&reference) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn upsampling_keeps_the_tone() {
        let out = resample(&tone(500.0, 8_000, 1.0), 8_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((rms(&out[1000..15_000]) - 0.3536).abs() < 0.02);
    }
}
