//! Minimal terminal harness for chatting through Codex OAuth.

mod agent_loop;
mod auth;
mod client;
mod config;
mod service;

use std::io;
use std::io::Write;
use std::time::Instant;

use agent_loop::AgentEvent;
use agent_loop::SessionId;
use anyhow::Context;
use anyhow::Result;
use auth::CodexAuth;
use client::ChatGptClient;
use config::AppConfig;
use mimalloc::MiMalloc;
use service::AgentService;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let AppConfig {
        client,
        context,
        models,
        output,
    } = AppConfig::from_env()?;
    let auth = CodexAuth::load_default()?;
    let client = ChatGptClient::new(auth, client)?;
    let mut service = AgentService::new(client, context, models);
    let session_id = SessionId::new(1);

    let Some(message) = message_from_args() else {
        return run_interactive_loop(&mut service, session_id, output).await;
    };

    print_agent_response(&mut service, session_id, output, message).await?;
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

async fn run_interactive_loop(
    service: &mut AgentService<ChatGptClient>,
    session_id: SessionId,
    output: config::OutputConfig,
) -> Result<()> {
    loop {
        let Some(input) = read_prompt()? else {
            return Ok(());
        };

        match input {
            InteractiveInput::Message(message) => {
                print_agent_response(service, session_id, output, message).await?;
            }
            InteractiveInput::ShowModel => {
                println!("Model: {}", service.session_model(session_id));
            }
            InteractiveInput::SetModel(model) => {
                match service.set_session_model(session_id, &model) {
                    Ok(model) => println!("Model: {model}"),
                    Err(error) => println!("Error: {error}"),
                }
            }
            InteractiveInput::ListModels => {
                println!("Models: {}", service.allowed_models().join(", "));
            }
        }
    }
}

async fn print_agent_response(
    service: &mut AgentService<ChatGptClient>,
    session_id: SessionId,
    output: config::OutputConfig,
    message: String,
) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "Assistant: ").context("failed to write assistant prompt")?;
    stdout.flush().context("failed to flush assistant prompt")?;

    let mut delta_writer = DeltaWriter::new(stdout, output);
    let response = service
        .submit_user_message(session_id, message, |event| match event {
            AgentEvent::TextDelta { delta, .. } => delta_writer.write_delta(delta),
            _ => Ok(()),
        })
        .await;
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum InteractiveInput {
    Message(String),
    ShowModel,
    SetModel(String),
    ListModels,
}

fn read_prompt() -> Result<Option<InteractiveInput>> {
    print!("You: ");
    io::stdout().flush().context("failed to flush prompt")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read message from stdin")?;
    Ok(parse_interactive_input(&input))
}

fn parse_interactive_input(input: &str) -> Option<InteractiveInput> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if input == "/models" {
        return Some(InteractiveInput::ListModels);
    }
    if input.split_whitespace().next() == Some("/model") {
        let model = input.trim_start_matches("/model").trim();
        return if model.is_empty() {
            Some(InteractiveInput::ShowModel)
        } else {
            Some(InteractiveInput::SetModel(model.to_string()))
        };
    }
    Some(InteractiveInput::Message(input.to_string()))
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

    #[test]
    fn parses_model_commands() {
        assert_eq!(parse_interactive_input(""), None);
        assert_eq!(
            parse_interactive_input("/model"),
            Some(InteractiveInput::ShowModel)
        );
        assert_eq!(
            parse_interactive_input("/model gpt-5.4"),
            Some(InteractiveInput::SetModel("gpt-5.4".to_string()))
        );
        assert_eq!(
            parse_interactive_input("/models"),
            Some(InteractiveInput::ListModels)
        );
        assert_eq!(
            parse_interactive_input("/help"),
            Some(InteractiveInput::Message("/help".to_string()))
        );
    }
}
