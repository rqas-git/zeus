//! Minimal terminal harness for chatting through Codex OAuth.

mod auth;
mod client;

use std::io;
use std::io::Write;

use anyhow::Context;
use anyhow::Result;
use auth::CodexAuth;
use client::ChatGptClient;
use client::ConversationMessage;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<()> {
    let auth = CodexAuth::load_default()?;
    let client = ChatGptClient::new(auth)?;

    let Some(message) = message_from_args() else {
        return run_interactive_loop(&client);
    };

    print_streamed_message(&client, &message)?;
    Ok(())
}

fn message_from_args() -> Option<String> {
    let message = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if !message.trim().is_empty() {
        Some(message)
    } else {
        None
    }
}

fn run_interactive_loop(client: &ChatGptClient) -> Result<()> {
    let mut conversation = Vec::new();
    loop {
        let Some(message) = read_prompt()? else {
            return Ok(());
        };

        conversation.push(ConversationMessage::user(message));
        let response = print_streamed_conversation(client, &conversation)?;
        conversation.push(ConversationMessage::assistant(response));
    }
}

fn print_streamed_message(client: &ChatGptClient, message: &str) -> Result<String> {
    print_streamed_response(|on_delta| client.stream_message(message, on_delta))
}

fn print_streamed_conversation(
    client: &ChatGptClient,
    conversation: &[ConversationMessage],
) -> Result<String> {
    print_streamed_response(|on_delta| client.stream_conversation(conversation, on_delta))
}

fn print_streamed_response(
    send: impl FnOnce(&mut dyn FnMut(&str) -> Result<()>) -> Result<String>,
) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "Assistant: ").context("failed to write assistant prompt")?;
    stdout.flush().context("failed to flush assistant prompt")?;

    let mut on_delta = |delta: &str| {
        stdout
            .write_all(delta.as_bytes())
            .context("failed to write assistant response")?;
        stdout.flush().context("failed to flush assistant response")
    };
    let response = send(&mut on_delta);
    writeln!(stdout).context("failed to finish assistant response")?;
    response
}

fn read_prompt() -> Result<Option<String>> {
    print!("You: ");
    io::stdout().flush().context("failed to flush prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read message from stdin")?;
    let input = input.trim().to_string();
    if input.is_empty() {
        Ok(None)
    } else {
        Ok(Some(input))
    }
}
