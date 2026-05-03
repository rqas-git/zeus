//! Minimal terminal harness for chatting through Codex OAuth.

mod agent_loop;
mod auth;
mod client;
mod config;

use std::io;
use std::io::Write;
use std::time::Instant;

use agent_loop::AgentEvent;
use agent_loop::AgentLoop;
use agent_loop::SessionId;
use anyhow::Context;
use anyhow::Result;
use auth::CodexAuth;
use client::ChatGptClient;
use config::AppConfig;
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<()> {
    let config = AppConfig::from_env()?;
    let auth = CodexAuth::load_default()?;
    let client = ChatGptClient::new(auth, config.client)?;
    let mut agent = AgentLoop::with_context_window(SessionId::new(1), config.context);

    let Some(message) = message_from_args() else {
        return run_interactive_loop(&client, &mut agent, config.output);
    };

    print_agent_response(&client, &mut agent, config.output, message)?;
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

fn run_interactive_loop(
    client: &ChatGptClient,
    agent: &mut AgentLoop,
    output: config::OutputConfig,
) -> Result<()> {
    loop {
        let Some(message) = read_prompt()? else {
            return Ok(());
        };

        print_agent_response(client, agent, output, message)?;
    }
}

fn print_agent_response(
    client: &ChatGptClient,
    agent: &mut AgentLoop,
    output: config::OutputConfig,
    message: String,
) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "Assistant: ").context("failed to write assistant prompt")?;
    stdout.flush().context("failed to flush assistant prompt")?;

    let session_id = agent.session_id();
    let mut delta_writer = DeltaWriter::new(stdout, output);
    let response = agent.submit_user_message(
        message,
        |conversation, on_delta| client.stream_conversation(conversation, session_id, on_delta),
        |event| match event {
            AgentEvent::TextDelta { delta, .. } => delta_writer.write_delta(delta),
            _ => Ok(()),
        },
    );
    delta_writer.finish_line()?;
    response
}

struct DeltaWriter<W> {
    writer: W,
    pending: String,
    flush_interval: std::time::Duration,
    flush_bytes: usize,
    last_flush: Instant,
}

impl<W> DeltaWriter<W>
where
    W: Write,
{
    fn new(writer: W, config: config::OutputConfig) -> Self {
        Self {
            writer,
            pending: String::new(),
            flush_interval: config.delta_flush_interval(),
            flush_bytes: config.delta_flush_bytes(),
            last_flush: Instant::now(),
        }
    }

    fn write_delta(&mut self, delta: &str) -> Result<()> {
        self.pending.push_str(delta);
        if self.pending.len() >= self.flush_bytes
            || self.last_flush.elapsed() >= self.flush_interval
        {
            self.flush_pending()?;
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<()> {
        self.flush_pending()?;
        writeln!(self.writer).context("failed to finish assistant response")?;
        self.writer
            .flush()
            .context("failed to flush assistant response")
    }

    fn flush_pending(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        self.writer
            .write_all(self.pending.as_bytes())
            .context("failed to write assistant response")?;
        self.writer
            .flush()
            .context("failed to flush assistant response")?;
        self.pending.clear();
        self.last_flush = Instant::now();
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn delta_writer_batches_until_threshold_or_finish() {
        let output = config::OutputConfig::new(Duration::from_secs(60), 8);
        let mut writer = DeltaWriter::new(Vec::new(), output);

        writer.write_delta("hello").unwrap();
        assert!(writer.writer.is_empty());

        writer.write_delta(" world").unwrap();
        assert_eq!(
            String::from_utf8(writer.writer.clone()).unwrap(),
            "hello world"
        );

        writer.write_delta("!").unwrap();
        writer.finish_line().unwrap();

        assert_eq!(String::from_utf8(writer.writer).unwrap(), "hello world!\n");
    }
}
