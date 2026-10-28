//! Streaming conversion of any input rate to whisper's 16 kHz.
//!
//! Windowed-sinc interpolation: one filter both band-limits (below the lower
//! Nyquist rate, so 48 kHz input does not alias into speech) and interpolates
//! the fractional position. The output instants fall on a fixed set of
//! fractional offsets (`output / gcd` of them), so the filter is tabulated
//! once per offset: the audio callback only multiplies and adds. Microphones
//! deliver blocks of arbitrary size, so the filter history carries across
//! calls and the output is identical however the input was split.

const HALF_TAPS: usize = 16; // per side, in units of the slower rate
const ROLLOFF: f64 = 0.92; // cutoff as a fraction of the lower Nyquist rate
/// Upper bound on tabulated offsets. Rates with more (e.g. 47999 Hz) share
/// the nearest one; 1/1024 of a sample is far below audibility.
const MAX_PHASES: usize = 1024;

pub struct Resampler {
    up: usize, // output instants per `down` input samples
    down: usize,
    phases: usize,     // rows of `table`
    taps: usize,       // columns of `table`
    half_width: usize, // filter half-length in input samples
    table: Vec<f32>,   // phases x taps coefficients
    history: Vec<f32>, // input samples not yet consumed by the filter
    position: usize,   // next output instant relative to history[0], in 1/up samples
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        assert!(
            input_rate > 0 && output_rate > 0,
            "sample rates must be positive"
        );
        let g = gcd(input_rate as usize, output_rate as usize);
        let (up, down) = (output_rate as usize / g, input_rate as usize / g);
        let step = down as f64 / up as f64; // input samples per output sample
        let cutoff = 0.5 * ROLLOFF * (1.0 / step).min(1.0); // cycles per input sample
        let half_width = (HALF_TAPS as f64 * step.max(1.0)).ceil() as usize;
        let taps = 2 * half_width;
        let phases = up.min(MAX_PHASES);
        let mut table = Vec::with_capacity(phases * taps);
        for phase in 0..phases {
            let frac = phase as f64 / phases as f64;
            for k in 0..taps {
                // tap k reads input center + k + 1 - half_width
                let d = k as f64 + 1.0 - half_width as f64 - frac;
                table.push(kernel(d, cutoff, half_width as f64) as f32);
            }
        }
        Resampler {
            up,
            down,
            phases,
            taps,
            half_width,
            table,
            // Leading silence lets the first output sample sit at time zero.
            history: vec![0.0; half_width],
            position: half_width * up,
        }
    }

    /// Feed input samples; returns every output sample now computable.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        self.process_into(input, &mut out);
        out
    }

    /// Like [`Self::process`], appending to `out` (no allocation once `out`
    /// has grown to the block size).
    pub fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.history.extend_from_slice(input);
        loop {
            let center = self.position / self.up;
            if center + self.half_width >= self.history.len() {
                break;
            }
            let phase = (self.position % self.up) * self.phases / self.up;
            let coefficients = &self.table[phase * self.taps..(phase + 1) * self.taps];
            let first = center + 1 - self.half_width;
            let samples = &self.history[first..first + self.taps];
            out.push(samples.iter().zip(coefficients).map(|(x, c)| x * c).sum());
            self.position += self.down;
        }
        // Drop history the filter can no longer reach.
        let keep_from = (self.position / self.up).saturating_sub(self.half_width);
        if keep_from > 0 {
            self.history.drain(..keep_from);
            self.position -= keep_from * self.up;
        }
    }
}

fn kernel(d: f64, cutoff: f64, w: f64) -> f64 {
    if d.abs() >= w {
        return 0.0;
    }
    let x = 2.0 * cutoff * d;
    let sinc = if x.abs() < 1e-9 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    };
    // Blackman window over [-w, w].
    let n = (d + w) / (2.0 * w);
    let tau = 2.0 * std::f64::consts::PI;
    let window = 0.42 - 0.5 * (tau * n).cos() + 0.08 * (2.0 * tau * n).cos();
    2.0 * cutoff * sinc * window
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
    fn odd_rates_share_the_nearest_tabulated_offset() {
        // 47999 Hz needs 16000 offsets; the table keeps 1024.
        for rate in [44_100, 47_999, 22_050] {
            let out = resample(&tone(1000.0, rate, 1.0), rate, 16_000);
            assert_eq!(out.len(), 16_000, "{rate}");
            assert!((rms(&out[1000..15_000]) - 0.3536).abs() < 0.02, "{rate}");
        }
    }

    #[test]
    fn upsampling_keeps_the_tone() {
        let out = resample(&tone(500.0, 8_000, 1.0), 8_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((rms(&out[1000..15_000]) - 0.3536).abs() < 0.02);
    }
}
