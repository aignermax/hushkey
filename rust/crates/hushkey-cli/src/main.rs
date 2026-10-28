//! `hushkey-cli`: hushkey's recognition from a terminal.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use hushkey_core::{
    model, read_wav_16k, AudioInput, Engine, EngineConfig, Event, EventSink, ListenConfig,
    ListenSession, Microphone, RecordConfig, RecordSession, TranscribeOptions, WavReplay,
};

#[derive(Parser)]
#[command(version, about = "Local speech recognition with whisper.cpp")]
struct Cli {
    #[command(flatten)]
    engine: EngineArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct EngineArgs {
    /// Model file, or a name (tiny, small, medium, large-v3-turbo, ...).
    #[arg(
        long,
        global = true,
        env = "HUSHKEY_MODEL",
        default_value = "large-v3-turbo"
    )]
    model: String,
    /// Language code; detected when omitted.
    #[arg(long, short, global = true)]
    language: Option<String>,
    /// Run on the CPU even when a GPU backend is built in.
    #[arg(long, global = true)]
    cpu: bool,
    /// GPU index.
    #[arg(long, global = true, default_value_t = 0)]
    gpu: i32,
    /// Mask pauses with the Silero VAD model (downloaded on first use).
    #[arg(long, global = true)]
    vad: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Transcribe a WAV file and print the text.
    Transcribe {
        wav: PathBuf,
        /// Print segment timestamps.
        #[arg(long)]
        segments: bool,
    },
    /// Record until Enter is pressed, showing a live preview.
    Record {
        /// Input device name (see `devices`).
        #[arg(long)]
        device: Option<String>,
        /// Record from a WAV file instead of the microphone.
        #[arg(long)]
        wav: Option<PathBuf>,
        /// Vocabulary or style hint for whisper.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Listen continuously and print every utterance (Enter stops; a WAV
    /// input ends by itself).
    Listen {
        /// Input device name (see `devices`).
        #[arg(long)]
        device: Option<String>,
        /// Listen to a WAV file instead of the microphone.
        #[arg(long)]
        wav: Option<PathBuf>,
        /// Vocabulary or style hint for whisper.
        #[arg(long)]
        prompt: Option<String>,
        /// Pause in milliseconds that ends an utterance.
        #[arg(long, default_value_t = 500)]
        end_silence: u32,
    },
    /// List input devices.
    Devices,
}

fn main() -> ExitCode {
    // SAFETY: first thing in main, before any other thread exists.
    unsafe { hushkey_core::init_process() };
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("hushkey-cli: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Devices => {
            for name in Microphone::devices().map_err(|e| e.to_string())? {
                println!("{name}");
            }
            Ok(())
        }
        Command::Transcribe { wav, segments } => {
            let engine = load(&cli.engine, false)?;
            let audio = read_wav_16k(&wav).map_err(|e| e.to_string())?;
            let t = engine
                .transcribe(&audio, &TranscribeOptions::accurate())
                .map_err(|e| e.to_string())?;
            if segments {
                for s in &t.segments {
                    println!(
                        "[{:>7.2} - {:>7.2}] {}",
                        s.start_ms as f64 / 1000.0,
                        s.end_ms as f64 / 1000.0,
                        s.text
                    );
                }
            } else {
                println!("{}", t.text);
            }
            Ok(())
        }
        Command::Record {
            device,
            wav,
            prompt,
        } => {
            let engine = Arc::new(load(&cli.engine, false)?);
            let input = input(device, wav.clone());
            let mut config = RecordConfig::default();
            config.options.prompt = prompt;
            let sink: EventSink = Arc::new(|event| match event {
                Event::Preview { text } => {
                    eprint!("\r\x1b[2K… {text}");
                    let _ = std::io::stderr().flush();
                }
                Event::Final { transcript } => {
                    eprint!("\r\x1b[2K");
                    println!("{}", transcript.text);
                }
                Event::Error { message } => eprintln!("\nerror: {message}"),
                _ => {}
            });
            let mut session =
                RecordSession::start(engine, input, config, sink).map_err(|e| e.to_string())?;
            eprintln!("recording, press Enter to stop");
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            session.stop().map_err(|e| e.to_string())?;
            Ok(())
        }
        Command::Listen {
            device,
            wav,
            prompt,
            end_silence,
        } => {
            let engine = Arc::new(load(&cli.engine, true)?);
            let ends_by_itself = wav.is_some();
            let input = input(device, wav);
            let mut config = ListenConfig::default().with_end_silence(end_silence);
            config.options.prompt = prompt;
            let sink: EventSink = Arc::new(|event| match event {
                Event::Preview { text } => {
                    eprint!("\r\x1b[2K… {text}");
                    let _ = std::io::stderr().flush();
                }
                Event::Utterance {
                    transcript,
                    start_ms,
                    end_ms,
                } => {
                    eprint!("\r\x1b[2K");
                    println!(
                        "[{:>7.2} - {:>7.2}] {}",
                        start_ms as f64 / 1000.0,
                        end_ms as f64 / 1000.0,
                        transcript.text
                    );
                }
                Event::Error { message } => eprintln!("\nerror: {message}"),
                _ => {}
            });
            let mut session =
                ListenSession::start(engine, input, config, sink).map_err(|e| e.to_string())?;
            if ends_by_itself {
                session.wait().map_err(|e| e.to_string())?;
            } else {
                eprintln!("listening, press Enter to stop");
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                session.stop().map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}

fn input(device: Option<String>, wav: Option<PathBuf>) -> Box<dyn AudioInput> {
    match wav {
        Some(path) => Box::new(WavReplay::new(path, true)),
        None => Box::new(Microphone::new(device)),
    }
}

/// `needs_vad`: listening finds utterances with it, so it is loaded even
/// without `--vad`.
fn load(args: &EngineArgs, needs_vad: bool) -> Result<Engine, String> {
    let model = model::ensure_whisper_model(&args.model).map_err(|e| e.to_string())?;
    let mut config = EngineConfig::new(model);
    config.use_gpu = !args.cpu;
    config.gpu_device = args.gpu;
    config.language = args.language.clone();
    if args.vad || needs_vad {
        config.vad_model = Some(model::ensure_vad_model().map_err(|e| e.to_string())?);
    }
    Engine::new(config).map_err(|e| e.to_string())
}
