use crate::{err, identity::key_id, Identity, NOISE};
use hushkey_core::{Segment, TranscribeOptions, Transcript};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};

pub(crate) const FRAME: usize = 65535;
pub(crate) const CHUNK: usize = 60000;
pub(crate) const CONTROL: usize = 16384;
pub(crate) const REPLY: usize = 2 * 1024 * 1024;
pub(crate) const SAMPLES: usize = 16000 * 600;
pub(crate) const IDLE: Duration = Duration::from_secs(2);
pub(crate) const APPROVAL: Duration = Duration::from_secs(120);
pub(crate) const COMPUTE: Duration = Duration::from_secs(180);
pub(crate) const FINAL_WAIT: Duration = Duration::from_secs(30);
pub(crate) const GLOBAL: Duration = Duration::from_secs(240);

pub(crate) struct Io {
    pub stream: TcpStream,
    pub until: Instant,
    pub idle: Duration,
}
impl Io {
    pub fn new(stream: TcpStream) -> Result<Self, String> {
        stream.set_nodelay(true).map_err(err)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(err)?;
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .map_err(err)?;
        Ok(Self {
            stream,
            until: Instant::now() + GLOBAL,
            idle: IDLE,
        })
    }
    fn check(&self, abort: Option<&dyn Fn() -> bool>, last: Instant) -> Result<(), String> {
        if abort.is_some_and(|f| f()) {
            return Err("request cancelled or server unavailable".into());
        }
        if Instant::now() >= self.until || last.elapsed() >= self.idle {
            return Err("network deadline exceeded".into());
        }
        Ok(())
    }
    fn read(&mut self, mut buf: &mut [u8], abort: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        let mut last = Instant::now();
        while !buf.is_empty() {
            self.check(abort, last)?;
            match self.stream.read(buf) {
                Ok(0) => return Err("peer disconnected".into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    last = Instant::now();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(format!("network read: {e}")),
            }
        }
        Ok(())
    }
    fn write(&mut self, mut buf: &[u8], abort: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        let mut last = Instant::now();
        while !buf.is_empty() {
            self.check(abort, last)?;
            match self.stream.write(buf) {
                Ok(0) => return Err("peer disconnected".into()),
                Ok(n) => {
                    buf = &buf[n..];
                    last = Instant::now();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(format!("network write: {e}")),
            }
        }
        Ok(())
    }
    pub fn recv(
        &mut self,
        max: usize,
        abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<u8>, String> {
        let mut hdr = [0; 4];
        self.read(&mut hdr, abort)?;
        let n = u32::from_be_bytes(hdr) as usize;
        if n == 0 || n > max {
            return Err("invalid or oversized frame".into());
        }
        let mut data = vec![0; n];
        self.read(&mut data, abort)?;
        Ok(data)
    }
    pub fn send(&mut self, data: &[u8], abort: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        if data.is_empty() || data.len() > FRAME {
            return Err("invalid outgoing frame size".into());
        }
        self.write(&(data.len() as u32).to_be_bytes(), abort)?;
        self.write(data, abort)
    }
}
pub(crate) struct Channel {
    pub io: Io,
    noise: snow::TransportState,
    pub remote: String,
    pub code: String,
}
impl Channel {
    pub fn handshake(
        mut io: Io,
        id: &Identity,
        initiator: bool,
        abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Self, String> {
        let b = snow::Builder::new(NOISE.parse().map_err(err)?)
            .local_private_key(&id.0.key)
            .map_err(err)?
            .prologue(b"hushkey-net/v2")
            .map_err(err)?;
        let mut h = if initiator {
            b.build_initiator()
        } else {
            b.build_responder()
        }
        .map_err(err)?;
        let mut out = [0; 1024];
        for turn in 0..3 {
            if (turn % 2 == 0) == initiator {
                let n = h.write_message(&[], &mut out).map_err(err)?;
                io.send(&out[..n], abort)?;
            } else {
                let message = io.recv(1024, abort)?;
                let n = h.read_message(&message, &mut out).map_err(err)?;
                if n != 0 {
                    return Err("unexpected handshake payload".into());
                }
            }
        }
        let remote = key_id(h.get_remote_static().ok_or("missing remote static key")?);
        if remote == id.id() {
            return Err("self connection rejected".into());
        }
        let hash = h.get_handshake_hash();
        let code = format!(
            "{:08}",
            u64::from_be_bytes(hash[..8].try_into().map_err(err)?) % 100_000_000
        );
        Ok(Self {
            io,
            remote,
            code,
            noise: h.into_transport_mode().map_err(err)?,
        })
    }
    pub fn send(&mut self, data: &[u8], abort: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        if data.len() > FRAME - 16 {
            return Err("plaintext frame too large".into());
        }
        let mut out = vec![0; data.len() + 16];
        let n = self.noise.write_message(data, &mut out).map_err(err)?;
        self.io.send(&out[..n], abort)
    }
    pub fn recv(
        &mut self,
        max: usize,
        abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<u8>, String> {
        let input = self.io.recv((max + 16).min(FRAME), abort)?;
        let mut out = vec![0; input.len()];
        let n = self.noise.read_message(&input, &mut out).map_err(err)?;
        if n > max {
            return Err("decrypted frame too large".into());
        }
        out.truncate(n);
        Ok(out)
    }
    pub fn send_msg(&mut self, msg: &Msg, abort: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        let bytes = serde_json::to_vec(msg).map_err(err)?;
        if bytes.len() > CONTROL {
            return Err("control message too large".into());
        }
        self.send(&bytes, abort)
    }
    pub fn recv_msg(&mut self, abort: Option<&dyn Fn() -> bool>) -> Result<Msg, String> {
        let msg: Msg = decode(&self.recv(CONTROL, abort)?)?;
        if let Msg::Error { message } = msg {
            return Err(format!(
                "remote: {}",
                message.chars().take(1024).collect::<String>()
            ));
        }
        Ok(msg)
    }
    pub fn error(&mut self, message: &str) {
        let _ = self.send_msg(
            &Msg::Error {
                message: message.chars().take(1024).collect(),
            },
            None,
        );
    }
}
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|_| "invalid protocol message".into())
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Msg {
    Probe { name: String },
    Pong,
    Hello { name: String, server_pinned: bool },
    Offer { name: String, compute_granted: bool },
    Decision { allowed: bool },
    Ready,
    Request { samples: usize, options: Options },
    Accepted,
    Reply { bytes: usize },
    Error { message: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Options {
    language: Option<String>,
    prompt: Option<String>,
    beam_size: u32,
    keep_pauses: bool,
}
impl Options {
    pub fn from_core(o: &TranscribeOptions) -> Result<Self, String> {
        let v = Self {
            language: o.language.clone(),
            prompt: o.prompt.clone(),
            beam_size: o.beam_size,
            keep_pauses: o.keep_pauses,
        };
        v.validate()?;
        Ok(v)
    }
    fn validate(&self) -> Result<(), String> {
        if self.language.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 32
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        }) || self
            .prompt
            .as_ref()
            .is_some_and(|s| s.len() > 8192 || s.contains('\0'))
            || self.beam_size > 32
        {
            return Err("invalid transcription options".into());
        }
        Ok(())
    }
    pub fn core(self) -> Result<TranscribeOptions, String> {
        self.validate()?;
        Ok(TranscribeOptions {
            language: self.language,
            prompt: self.prompt,
            beam_size: self.beam_size,
            keep_pauses: self.keep_pauses,
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultDto {
    text: String,
    segments: Vec<SegmentDto>,
    language: Option<String>,
    no_speech: f32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentDto {
    start_ms: i64,
    end_ms: i64,
    text: String,
}
pub(crate) fn encode_result(t: Transcript) -> Result<Vec<u8>, String> {
    if t.text.len() > REPLY
        || t.segments.len() > 20000
        || t.segments.iter().map(|s| s.text.len()).sum::<usize>() > REPLY
        || !t.no_speech.is_finite()
        || t.language.as_ref().is_some_and(|s| s.len() > 32)
    {
        return Err("invalid or oversized transcript".into());
    }
    let d = ResultDto {
        text: t.text,
        segments: t
            .segments
            .into_iter()
            .map(|s| SegmentDto {
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: s.text,
            })
            .collect(),
        language: t.language,
        no_speech: t.no_speech,
    };
    let b = serde_json::to_vec(&d).map_err(err)?;
    if b.len() > REPLY {
        return Err("transcript reply too large".into());
    }
    Ok(b)
}
pub(crate) fn decode_result(b: &[u8]) -> Result<Transcript, String> {
    if b.len() > REPLY {
        return Err("transcript reply too large".into());
    }
    let d: ResultDto = decode(b)?;
    if d.segments.len() > 20000
        || !d.no_speech.is_finite()
        || d.language.as_ref().is_some_and(|s| s.len() > 32)
    {
        return Err("invalid transcript reply".into());
    }
    Ok(Transcript {
        text: d.text,
        segments: d
            .segments
            .into_iter()
            .map(|s| Segment {
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: s.text,
            })
            .collect(),
        language: d.language,
        no_speech: d.no_speech,
    })
}
