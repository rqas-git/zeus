//! Minimal terminal harness for chatting through Codex OAuth.

mod agent_loop;
mod auth;
mod client;

use std::io;
use std::io::Write;

use agent_loop::AgentEvent;
use agent_loop::AgentLoop;
use agent_loop::SessionId;
use anyhow::Context;
use anyhow::Result;
use auth::CodexAuth;
use client::ChatGptClient;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<()> {
    let auth = CodexAuth::load_default()?;
    let client = ChatGptClient::new(auth)?;
    let mut agent = AgentLoop::new(SessionId::new(1));

    let Some(message) = message_from_args() else {
        return run_interactive_loop(&client, &mut agent);
    };

    print_agent_response(&client, &mut agent, message)?;
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

fn run_interactive_loop(client: &ChatGptClient, agent: &mut AgentLoop) -> Result<()> {
    loop {
        let Some(message) = read_prompt()? else {
            return Ok(());
        };

        print_agent_response(client, agent, message)?;
    }
}

fn print_agent_response(
    client: &ChatGptClient,
    agent: &mut AgentLoop,
    message: String,
) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "Assistant: ").context("failed to write assistant prompt")?;
    stdout.flush().context("failed to flush assistant prompt")?;

    let response = agent.submit_user_message(
        message,
        |conversation, on_delta| client.stream_conversation(conversation, on_delta),
        |event| match event {
            AgentEvent::TextDelta { delta, .. } => {
                stdout
                    .write_all(delta.as_bytes())
                    .context("failed to write assistant response")?;
                stdout.flush().context("failed to flush assistant response")
            }
            _ => Ok(()),
        },
    );
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
