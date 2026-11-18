//! Manual smoke client. TEST ONLY: automatically accepts the comparison code.
use hushkey_core::{read_wav_16k, TranscribeOptions};
use hushkey_net::{discover, Client, Identity, Peer};
use std::{path::Path, sync::Arc, time::Duration};

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[2] == "discover" {
        let identity = Identity::load_or_create(Path::new(&args[1]))?;
        let peers = discover(&identity, Duration::from_secs(2))?;
        println!(
            "{}",
            serde_json::to_string(&peers).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if args.len() != 6 {
        return Err("usage: peer STATE_DIR discover | peer STATE_DIR ADDRESS ID NAME WAV\nTEST ONLY: client confirmation is automatic; approve the matching code on the host.".into());
    }
    let identity = Identity::load_or_create(Path::new(&args[1]))?;
    let peer = Peer {
        address: args[2]
            .parse()
            .map_err(|e| format!("invalid address: {e}"))?,
        id: args[3].clone(),
        name: args[4].clone(),
    };
    let audio = read_wav_16k(Path::new(&args[5])).map_err(|e| e.to_string())?;
    let client = Client::connect(
        identity,
        &peer,
        Arc::new(|approval| {
            println!(
                "TEST ONLY: auto-confirming SAS {} for {} ({})",
                approval.code, approval.peer.name, approval.peer.id
            );
            true
        }),
    )?;
    println!(
        "Encrypted pairing confirmed; sending {} samples",
        audio.len()
    );
    let transcript = client.transcribe(&audio, &TranscribeOptions::accurate(), None)?;
    println!("{}", transcript.text);
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
