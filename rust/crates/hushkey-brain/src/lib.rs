//! Brains for hushkey's listening mode.
//!
//! [`OpenAiChat`] talks to any OpenAI-compatible `/v1/chat/completions`
//! server: llama.cpp's server, vLLM, Ollama, or an online provider.
//!
//! Another assistant is reached by implementing [`Brain`] yourself: the
//! trait is hushkey's interface for it, and [`OpenAiChat`] below is a
//! worked example. Through the C ABI, a brain is a pair of callbacks.
//!
//! Nothing leaves the machine unless a brain is configured with a URL.

use std::time::Duration;

use hushkey_core::{Brain, Reply};
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

fn agent(ca_cert_pem: Option<&[u8]>, timeout: Duration) -> Result<ureq::Agent, String> {
    use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
    // The operating system's TLS (Schannel, OpenSSL, Security.framework):
    // it trusts the system's certificates, company CAs included, and accepts
    // a self-signed server certificate marked as a CA, which rustls'
    // stricter checks refuse (CaUsedAsEndEntity).
    let roots = match ca_cert_pem {
        Some(pem) => RootCerts::new_with_certs(&[
            Certificate::from_pem(pem).map_err(|e| format!("CA certificate: {e}"))?
        ]),
        None => RootCerts::PlatformVerifier,
    };
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        // Error statuses come back as responses: their body says why.
        .http_status_as_error(false)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .root_certs(roots)
                .build(),
        );
    Ok(config.build().into())
}

fn base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// How long a busy server (429) is waited for: a server answering one chat
/// at a time can be held by a single request for minutes.
const BUSY_PATIENCE: Duration = Duration::from_secs(180);

/// Post `body`, retrying while the server answers 429 (busy): honouring
/// Retry-After, else backing off up to 10 s, for at most [`BUSY_PATIENCE`].
fn post(
    agent: &ureq::Agent,
    url: &str,
    token: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let started = std::time::Instant::now();
    let mut wait = Duration::from_millis(500);
    loop {
        let mut request = agent.post(url);
        if let Some(token) = token {
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        let mut response = request.send_json(body).map_err(|e| format!("{url}: {e}"))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| Duration::from_secs(s.min(30)));
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{url}: {e}"))?;
        if status == 429 && started.elapsed() < BUSY_PATIENCE {
            std::thread::sleep(retry_after.unwrap_or(wait));
            wait = (wait * 2).min(Duration::from_secs(10));
            continue;
        }
        if !(200..300).contains(&status) {
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").or(v.get("detail")).map(|d| d.to_string()))
                .unwrap_or(text);
            return Err(format!("{url}: HTTP {status}: {detail}"));
        }
        return serde_json::from_str(&text).map_err(|e| format!("{url}: not JSON: {e}"));
    }
}

/// `Some("<hidden>")` for a set secret: Debug output must not leak tokens.
fn redacted(secret: &Option<String>) -> Option<&'static str> {
    secret.as_ref().map(|_| "<hidden>")
}

/// Settings for [`OpenAiChat`].
#[derive(Clone, Default)]
pub struct OpenAiConfig {
    /// Base URL without `/v1`, e.g. `http://127.0.0.1:8080` (llama.cpp),
    /// `http://127.0.0.1:11434` (Ollama), `https://api.openai.com`.
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Instructions sent first with every request.
    pub system_prompt: Option<String>,
    /// Earlier messages kept as context, in user/assistant pairs (an odd
    /// number is rounded down).
    pub max_history: usize,
    pub ca_cert_pem: Option<Vec<u8>>,
}

impl std::fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("api_key", &redacted(&self.api_key))
            .field("system_prompt", &self.system_prompt)
            .field("max_history", &self.max_history)
            .finish()
    }
}

pub struct OpenAiChat {
    agent: ureq::Agent,
    url: String,
    config: OpenAiConfig,
    history: Vec<Message>,
}

impl OpenAiChat {
    pub fn new(config: OpenAiConfig) -> Result<OpenAiChat, String> {
        if config.url.is_empty() || config.model.is_empty() {
            return Err("an OpenAI-compatible brain needs a URL and a model".into());
        }
        Ok(OpenAiChat {
            agent: agent(config.ca_cert_pem.as_deref(), Duration::from_secs(300))?,
            url: format!("{}/v1/chat/completions", base(&config.url)),
            config,
            history: Vec::new(),
        })
    }
}

impl OpenAiChat {
    /// Drop the oldest pairs beyond `max_history`, keeping user/assistant
    /// alternation: strict chat templates reject a leading assistant turn.
    fn trim_history(&mut self) {
        let keep = self.config.max_history / 2 * 2;
        let excess = self.history.len().saturating_sub(keep);
        self.history.drain(..excess / 2 * 2 + excess % 2 * 2);
    }
}

impl Brain for OpenAiChat {
    fn reply(&mut self, utterance: &str) -> Result<Reply, String> {
        let mut messages = Vec::new();
        if let Some(system) = &self.config.system_prompt {
            messages.push(Message {
                role: "system",
                content: system.clone(),
            });
        }
        self.trim_history();
        messages.extend(self.history.iter().cloned());
        messages.push(Message {
            role: "user",
            content: utterance.to_string(),
        });
        let response = post(
            &self.agent,
            &self.url,
            self.config.api_key.as_deref(),
            &json!({"model": self.config.model, "messages": messages}),
        )?;
        let reply = response
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| format!("{}: no choices[0].message.content in {response}", self.url))?
            .to_string();
        self.history.push(Message {
            role: "user",
            content: utterance.to_string(),
        });
        self.history.push(Message {
            role: "assistant",
            content: reply.clone(),
        });
        self.trim_history();
        Ok(Reply {
            text: reply,
            details: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-request-at-a-time HTTP server answering with `respond(path,
    /// body)` → (status, json); the requests it saw go to the channel.
    fn server(
        mut respond: impl FnMut(&str, &serde_json::Value) -> (u16, String) + Send + 'static,
    ) -> (
        String,
        mpsc::Receiver<(String, serde_json::Value, Option<String>)>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let (mut length, mut auth) = (0usize, None);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => auth = Some(value.trim().to_string()),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let (status, reply) = respond(&path, &body);
                let _ = tx.send((path, body, auth));
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (url, rx)
    }

    #[test]
    fn openai_chat_sends_system_prompt_and_bounded_history() {
        let (url, seen) = server(|_, body| {
            let n = body["messages"].as_array().unwrap().len();
            (
                200,
                format!(
                    r#"{{"choices":[{{"message":{{"role":"assistant","content":"{n} messages"}}}}]}}"#
                ),
            )
        });
        let mut chat = OpenAiChat::new(OpenAiConfig {
            url,
            model: "qwen".into(),
            api_key: Some("k".into()),
            system_prompt: Some("Be brief.".into()),
            max_history: 2,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(chat.reply("a").unwrap().text, "2 messages"); // system + user
        assert_eq!(chat.reply("b").unwrap().text, "4 messages"); // + one pair
        assert_eq!(chat.reply("c").unwrap().text, "4 messages"); // history capped at 2
        let (path, body, auth) = seen.try_iter().last().unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], "qwen");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(auth.as_deref(), Some("Bearer k"));
    }

    #[test]
    fn a_bad_certificate_is_refused_up_front() {
        let err = OpenAiChat::new(OpenAiConfig {
            url: "https://127.0.0.1:8443".into(),
            // A model as well: unlike the config it replaced, this one
            // insists on both before it looks at the certificate.
            model: "a-model".into(),
            ca_cert_pem: Some(b"not a certificate".to_vec()),
            ..Default::default()
        })
        .err()
        .unwrap();
        assert!(err.contains("CA certificate"), "{err}");
    }

    #[test]
    fn openai_history_keeps_pairs_with_an_odd_limit() {
        let (url, seen) = server(|_, _| {
            (
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#.into(),
            )
        });
        let mut chat = OpenAiChat::new(OpenAiConfig {
            url,
            model: "m".into(),
            max_history: 3,
            ..Default::default()
        })
        .unwrap();
        for q in ["a", "b", "c", "d"] {
            chat.reply(q).unwrap();
        }
        let (_, body, _) = seen.try_iter().last().unwrap();
        let roles: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
    }

    #[test]
    fn debug_output_hides_secrets() {
        let openai = format!(
            "{:?}",
            OpenAiConfig {
                api_key: Some("sk-secret".into()),
                ..Default::default()
            }
        );
        assert!(!openai.contains("sk-secret") && openai.contains("<hidden>"));
    }
}
