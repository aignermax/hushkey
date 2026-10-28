//! Audio inputs delivering 16 kHz mono f32 blocks.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat};

use crate::error::{Error, Result};
use crate::resample::{resample, Resampler};

/// whisper.cpp's input rate.
pub const SAMPLE_RATE: u32 = 16_000;

/// Receives 16 kHz mono samples, in order, in blocks of any size.
pub type SamplesFn = Box<dyn FnMut(&[f32]) + Send>;

/// Where an input delivers its audio. Called from the input's own thread.
pub struct AudioSink {
    /// 16 kHz mono samples, in order, in blocks of any size.
    pub on_samples: SamplesFn,
    /// The input failed; no more samples will follow.
    pub on_error: Box<dyn FnMut(String) + Send>,
    /// A finite input (a file) delivered everything it had.
    pub on_end: Box<dyn FnOnce() + Send>,
}

/// A source of speech: the microphone, a file, or anything a host provides.
pub trait AudioInput: Send {
    /// Begin delivering audio to `sink` from a background thread.
    fn start(&mut self, sink: AudioSink) -> Result<()>;
    /// Stop delivering; no sink call happens after this returns.
    fn stop(&mut self);
}

/// The system's microphone (or a named input device).
pub struct Microphone {
    device: Option<String>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Microphone {
    /// `device`: a name as the OS lists it; `None` for the default input.
    pub fn new(device: Option<String>) -> Self {
        Microphone {
            device,
            stop: None,
            thread: None,
        }
    }

    /// Names of the available input devices.
    pub fn devices() -> Result<Vec<String>> {
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|e| Error::Audio(e.to_string()))?;
        Ok(devices
            .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
            .collect())
    }
}

impl AudioInput for Microphone {
    fn start(&mut self, sink: AudioSink) -> Result<()> {
        if self.thread.is_some() {
            return Err(Error::InvalidArgument(
                "the microphone is already started".into(),
            ));
        }
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let device_name = self.device.clone();
        // cpal streams are not Send on every platform: the stream lives and
        // dies on this thread.
        let thread = std::thread::Builder::new()
            .name("hushkey-mic".into())
            .spawn(move || {
                let stream = match open_stream(device_name, sink) {
                    Ok(stream) => stream,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    let _ = ready_tx.send(Err(Error::Audio(e.to_string())));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv(); // until stop() or the Microphone is dropped
                drop(stream);
            })
            .map_err(Error::Io)?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.stop = Some(stop_tx);
                self.thread = Some(thread);
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::Audio("the audio thread ended during startup".into()))
            }
        }
    }

    fn stop(&mut self) {
        self.stop.take(); // dropping the sender wakes the thread
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.stop();
    }
}

fn open_stream(device_name: Option<String>, sink: AudioSink) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = match &device_name {
        None => host
            .default_input_device()
            .ok_or_else(|| Error::Audio("no microphone found".into()))?,
        Some(name) => host
            .input_devices()
            .map_err(|e| Error::Audio(e.to_string()))?
            .find(|d| {
                d.description()
                    .map(|desc| desc.name() == name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| Error::Audio(format!("no input device named {name:?}")))?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| Error::Audio(e.to_string()))?;
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    let AudioSink {
        on_samples,
        mut on_error,
        ..
    } = sink;
    let err_fn = move |e: cpal::Error| on_error(e.to_string());
    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            build::<f32>(&device, config.into(), channels, rate, on_samples, err_fn)
        }
        SampleFormat::I16 => {
            build::<i16>(&device, config.into(), channels, rate, on_samples, err_fn)
        }
        SampleFormat::I32 => {
            build::<i32>(&device, config.into(), channels, rate, on_samples, err_fn)
        }
        SampleFormat::U16 => {
            build::<u16>(&device, config.into(), channels, rate, on_samples, err_fn)
        }
        SampleFormat::I8 => build::<i8>(&device, config.into(), channels, rate, on_samples, err_fn),
        other => return Err(Error::Audio(format!("unsupported sample format {other}"))),
    };
    stream.map_err(|e| Error::Audio(e.to_string()))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    rate: u32,
    mut on_samples: SamplesFn,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> std::result::Result<cpal::Stream, cpal::Error>
where
    T: Sample + cpal::SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let mut resampler = (rate != SAMPLE_RATE).then(|| Resampler::new(rate, SAMPLE_RATE));
    let mut mono = Vec::new();
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            mono.extend(data.chunks(channels).map(|frame| {
                frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32
            }));
            match resampler.as_mut() {
                Some(r) => on_samples(&r.process(&mono)),
                None => on_samples(&mono),
            }
        },
        err_fn,
        None,
    )
}

/// Plays a WAV file as if it were spoken into a microphone: for tests, demos
/// and hosts that already have recorded audio.
pub struct WavReplay {
    path: PathBuf,
    realtime: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WavReplay {
    /// `realtime`: pace delivery like a live microphone; otherwise deliver as
    /// fast as the consumer takes it.
    pub fn new(path: impl Into<PathBuf>, realtime: bool) -> Self {
        WavReplay {
            path: path.into(),
            realtime,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl AudioInput for WavReplay {
    fn start(&mut self, sink: AudioSink) -> Result<()> {
        if self.thread.is_some() {
            return Err(Error::InvalidArgument(
                "the replay is already started".into(),
            ));
        }
        let samples = read_wav_16k(&self.path)?;
        let realtime = self.realtime;
        let stop = self.stop.clone();
        stop.store(false, Ordering::SeqCst);
        let AudioSink {
            mut on_samples,
            on_end,
            ..
        } = sink;
        self.thread = Some(
            std::thread::Builder::new()
                .name("hushkey-replay".into())
                .spawn(move || {
                    const BLOCK: usize = SAMPLE_RATE as usize / 50; // 20 ms
                    let began = Instant::now();
                    for (i, block) in samples.chunks(BLOCK).enumerate() {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        if realtime {
                            let due = began + Duration::from_millis(20 * i as u64);
                            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                                std::thread::sleep(wait);
                            }
                        }
                        on_samples(block);
                    }
                    if !stop.load(Ordering::SeqCst) {
                        on_end();
                    }
                })
                .map_err(Error::Io)?,
        );
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for WavReplay {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Read a WAV file of any rate, channel count and sample format as 16 kHz mono.
pub fn read_wav_16k(path: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| Error::Audio(format!("{}: {e}", path.display())))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::Audio(e.to_string()))?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::Audio(e.to_string()))?
        }
    };
    let mono: Vec<f32> = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok(resample(&mono, spec.sample_rate, SAMPLE_RATE))
}

/// Root mean square level, for "nothing was said" checks.
pub(crate) fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn stereo_48k_file_reads_as_16k_mono() {
        let dir = std::env::temp_dir().join(format!("hk-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");
        // one second, left at +0.25, right at -0.25: mono is silence
        let samples: Vec<i16> = (0..48_000).flat_map(|_| [8192i16, -8192]).collect();
        write_wav(&path, 48_000, 2, &samples);
        let mono = read_wav_16k(&path).unwrap();
        assert_eq!(mono.len(), 16_000);
        assert!(rms(&mono) < 1e-3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replay_delivers_every_sample_then_ends() {
        let dir = std::env::temp_dir().join(format!("hk-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("half.wav");
        write_wav(&path, 16_000, 1, &vec![1000i16; 8_000]);
        let (tx, rx) = mpsc::channel();
        let (end_tx, end_rx) = mpsc::channel();
        let mut replay = WavReplay::new(&path, false);
        replay
            .start(AudioSink {
                on_samples: Box::new(move |s| tx.send(s.len()).unwrap()),
                on_error: Box::new(|e| panic!("{e}")),
                on_end: Box::new(move || end_tx.send(()).unwrap()),
            })
            .unwrap();
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        replay.stop();
        assert_eq!(rx.try_iter().sum::<usize>(), 8_000);
        std::fs::remove_dir_all(&dir).ok();
    }
}
