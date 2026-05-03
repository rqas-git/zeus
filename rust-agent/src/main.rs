//! Minimal terminal harness for sending one message through Codex OAuth.

mod auth;
mod client;

use std::io;
use std::io::Write;

use anyhow::Context;
use anyhow::Result;
use auth::CodexAuth;
use client::ChatGptClient;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<()> {
    let message = message_from_args_or_stdin()?;
    let auth = CodexAuth::load_default()?;
    let client = ChatGptClient::new(auth)?;
    let response = client.send_message(&message)?;

    println!("Assistant: {response}");
    Ok(())
}

fn message_from_args_or_stdin() -> Result<String> {
    let message = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if !message.trim().is_empty() {
        return Ok(message);
    }

    print!("You: ");
    io::stdout().flush().context("failed to flush prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read message from stdin")?;
    let input = input.trim().to_string();
    anyhow::ensure!(!input.is_empty(), "message cannot be empty");
    Ok(input)
}
