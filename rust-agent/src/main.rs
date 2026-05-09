//! Minimal terminal harness for chatting through rust-agent auth.

mod agent_loop;
mod auth;
#[cfg(test)]
mod bench_support;
mod client;
mod compaction;
mod config;
mod server;
mod service;
mod storage;
#[cfg(test)]
mod test_http;
mod tools;
mod workspace;

use std::io;
use std::io::Write;
use std::time::Instant;

use agent_loop::AgentEvent;
use agent_loop::CacheHealth;
use agent_loop::SessionId;
use agent_loop::TokenUsage;
use anyhow::Context;
use anyhow::Result;
use auth::AuthManager;
use auth::AuthStatus;
use client::ChatGptClient;
use config::AppConfig;
use mimalloc::MiMalloc;
use service::AgentService;
use storage::SessionDatabase;
use tools::ToolRegistry;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    match parse_cli_command(std::env::args().skip(1).collect())? {
        CliCommand::Interactive => run_agent(None).await,
        CliCommand::Prompt(message) => run_agent(Some(message)).await,
        CliCommand::Serve => run_server().await,
        CliCommand::Contract => print_contract(),
        CliCommand::LoginDeviceCode => run_device_code_login().await,
        CliCommand::LoginStatus => run_login_status().await,
        CliCommand::Logout => run_logout().await,
    }
}

async fn run_agent(message: Option<String>) -> Result<()> {
    let AppConfig {
        client: client_config,
        compaction,
        context,
        models,
        output,
        storage,
        telemetry,
        tools: tool_config,
        ..
    } = AppConfig::from_env()?;
    let session_id = SessionId::new(1);
    let tools = ToolRegistry::for_root_with_policy_and_search_concurrency(
        tool_config.workspace_root(),
        tool_config.policy(),
        tool_config.search_concurrency(),
    );

    match message {
        Some(message) => {
            let auth = AuthManager::new_default()?;
            let client = ChatGptClient::new(auth, client_config)?;
            let database = SessionDatabase::open(storage.database_path())?;
            let service = AgentService::with_tools(client, context, models, tools)
                .with_database(database)
                .with_compaction(compaction);
            print_agent_response(&service, session_id, output, telemetry, message).await
        }
        None => {
            let _search_index_warmup = tools.spawn_search_index_warmup();
            let auth = AuthManager::new_default()?;
            let client = ChatGptClient::new(auth, client_config)?;
            let database = SessionDatabase::open(storage.database_path())?;
            let service = AgentService::with_tools(client, context, models, tools)
                .with_database(database)
                .with_compaction(compaction);
            run_interactive_loop(&service, session_id, output, telemetry).await
        }
    }
}

async fn run_server() -> Result<()> {
    let AppConfig {
        client: client_config,
        compaction,
        context,
        models,
        server,
        storage,
        tools: tool_config,
        ..
    } = AppConfig::from_env()?;
    let tools = ToolRegistry::for_root_with_policy_and_search_concurrency(
        tool_config.workspace_root(),
        tool_config.policy(),
        tool_config.search_concurrency(),
    );
    let workspace_root = tools.root().to_path_buf();
    let _search_index_warmup = tools.spawn_search_index_warmup();
    let auth = AuthManager::new_default()?;
    let client = ChatGptClient::new(auth, client_config)?;
    let database = SessionDatabase::open(storage.database_path())?;
    let service = AgentService::with_tools(client, context, models, tools)
        .with_database(database)
        .with_compaction(compaction)
        .with_session_limit(server.max_sessions());
    server::serve(service, server, workspace_root).await
}

fn print_contract() -> Result<()> {
    println!("{}", server::zeus_api_contract_pretty()?);
    Ok(())
}

async fn run_device_code_login() -> Result<()> {
    let auth = AuthManager::new_default()?;
    let login = auth.start_device_login().await?;
    println!("Open this URL and enter this code:");
    println!("{}", login.verification_url());
    println!("{}", login.user_code());
    println!("Waiting for authorization...");
    let credentials = auth.complete_device_login(login).await?;
    println!("Logged in. account_id={}", credentials.account_id());
    Ok(())
}

async fn run_login_status() -> Result<()> {
    let auth = AuthManager::new_default()?;
    match auth.status().await? {
        AuthStatus::LoggedOut => {
            println!("Not logged in. Run: rust-agent login --device-code");
        }
        AuthStatus::LoggedIn {
            account_id,
            expires_at_unix,
        } => {
            println!("Logged in. account_id={account_id} expires_at_unix={expires_at_unix}");
        }
    }
    Ok(())
}

async fn run_logout() -> Result<()> {
    let auth = AuthManager::new_default()?;
    let result = auth.logout().await?;
    if let Some(error) = result.revoke_error() {
        println!("Logged out locally. Token revoke failed: {error}");
    } else if result.removed() {
        println!("Logged out.");
    } else {
        println!("Not logged in.");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CliCommand {
    Interactive,
    Prompt(String),
    Serve,
    Contract,
    LoginDeviceCode,
    LoginStatus,
    Logout,
}

fn parse_cli_command(args: Vec<String>) -> Result<CliCommand> {
    let Some(first) = args.first() else {
        return Ok(CliCommand::Interactive);
    };

    match first.as_str() {
        "login" => parse_login_command(&args[1..]),
        "serve" => {
            anyhow::ensure!(args.len() == 1, "usage: rust-agent serve");
            Ok(CliCommand::Serve)
        }
        "contract" => {
            anyhow::ensure!(args.len() == 1, "usage: rust-agent contract");
            Ok(CliCommand::Contract)
        }
        "logout" => {
            anyhow::ensure!(args.len() == 1, "usage: rust-agent logout");
            Ok(CliCommand::Logout)
        }
        _ => {
            let message = args.join(" ");
            if message.trim().is_empty() {
                Ok(CliCommand::Interactive)
            } else {
                Ok(CliCommand::Prompt(message))
            }
        }
    }
}

fn parse_login_command(args: &[String]) -> Result<CliCommand> {
    match args {
        [] => Ok(CliCommand::LoginDeviceCode),
        [device_code] if device_code == "--device-code" => Ok(CliCommand::LoginDeviceCode),
        [status] if status == "status" || status == "--status" => Ok(CliCommand::LoginStatus),
        _ => {
            anyhow::bail!("usage: rust-agent login [--device-code|status]");
        }
    }
}

async fn run_interactive_loop(
    service: &AgentService<ChatGptClient>,
    session_id: SessionId,
    output: config::OutputConfig,
    telemetry: config::TelemetryConfig,
) -> Result<()> {
    loop {
        let Some(input) = read_prompt()? else {
            return Ok(());
        };

        match input {
            InteractiveInput::Message(message) => {
                print_agent_response(service, session_id, output, telemetry, message).await?;
            }
            InteractiveInput::ShowModel => {
                println!("Model: {}", service.session_model(session_id).await?);
            }
            InteractiveInput::SetModel(model) => {
                match service.set_session_model(session_id, &model).await {
                    Ok(model) => println!("Model: {model}"),
                    Err(error) => println!("Error: {error}"),
                }
            }
            InteractiveInput::ListModels => {
                println!("Models: {}", service.allowed_models().join(", "));
            }
            InteractiveInput::Compact(instructions) => {
                compact_session(service, session_id, instructions.as_deref()).await?;
            }
        }
    }
}

async fn compact_session(
    service: &AgentService<ChatGptClient>,
    session_id: SessionId,
    instructions: Option<&str>,
) -> Result<()> {
    let result = service
        .compact_session(session_id, instructions, None, |_| Ok(()))
        .await?;
    println!(
        "Compacted {} tokens; kept from message {}.",
        result.tokens_before,
        result.first_kept_message_id.get()
    );
    Ok(())
}

async fn print_agent_response(
    service: &AgentService<ChatGptClient>,
    session_id: SessionId,
    output: config::OutputConfig,
    telemetry: config::TelemetryConfig,
    message: String,
) -> Result<()> {
    let mut stdout = io::stdout();
    write!(stdout, "Assistant: ").context("failed to write assistant prompt")?;
    stdout.flush().context("failed to flush assistant prompt")?;

    let mut delta_writer = DeltaWriter::new(stdout, output);
    let mut cache_health = None;
    let response = service
        .submit_user_message(session_id, message, |event| match event {
            AgentEvent::TextDelta { delta, .. } => delta_writer.write_delta(delta),
            AgentEvent::CacheHealth {
                cache_health: health,
                ..
            } => {
                if telemetry.cache_health() {
                    cache_health = Some(health.clone());
                }
                Ok(())
            }
            _ => Ok(()),
        })
        .await;
    delta_writer.finish_line()?;
    if telemetry.cache_health() {
        if let Some(cache_health) = &cache_health {
            log_cache_health(cache_health);
        }
    }
    response
}

fn log_cache_health(cache_health: &CacheHealth) {
    let usage = cache_health
        .usage
        .map(token_usage_log_fields)
        .unwrap_or(serde_json::Value::Null);
    let entry = serde_json::json!({
        "event": "cache.health.observed",
        "message": "cache health observed for {model.name}",
        "fields": {
            "cache.status": cache_health.cache_status.as_str(),
            "model.name": cache_health.model,
            "prompt.cache_key": cache_health.prompt_cache_key,
            "prompt.stable_prefix_hash": format!("{:016x}", cache_health.stable_prefix_hash),
            "prompt.stable_prefix_bytes": cache_health.stable_prefix_bytes,
            "request.input_hash": format!("{:016x}", cache_health.request_input_hash),
            "request.message_count": cache_health.message_count,
            "request.input_bytes": cache_health.input_bytes,
            "response.id": cache_health.response_id,
            "usage": usage,
        },
    });
    eprintln!("{entry}");
}

fn token_usage_log_fields(usage: TokenUsage) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "cached_input_tokens": usage.cached_input_tokens,
        "cache_hit_ratio": usage.cache_hit_ratio(),
        "output_tokens": usage.output_tokens,
        "reasoning_output_tokens": usage.reasoning_output_tokens,
        "total_tokens": usage.total_tokens,
    })
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
            pending: String::with_capacity(config.delta_flush_bytes().min(64 * 1024)),
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
    Compact(Option<String>),
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
    if input.split_whitespace().next() == Some("/compact") {
        let instructions = input.trim_start_matches("/compact").trim();
        return Some(InteractiveInput::Compact(
            (!instructions.is_empty()).then(|| instructions.to_string()),
        ));
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
    use crate::bench_support::DurationSummary;

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
    #[ignore = "release-mode delta writer benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_delta_writer_many_small_deltas() {
        const DELTAS: usize = 200_000;
        const SAMPLES: usize = 15;

        let output = config::OutputConfig::new(Duration::from_secs(60), 4096);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let mut writer = DeltaWriter::new(Vec::with_capacity(DELTAS + 1), output);
            let started = Instant::now();
            for _ in 0..DELTAS {
                writer.write_delta("x").unwrap();
            }
            writer.finish_line().unwrap();
            let elapsed = started.elapsed();

            output_bytes = writer.writer.len();
            std::hint::black_box(&writer.writer);
            assert_eq!(output_bytes, DELTAS + 1);
            samples.push(elapsed);
        }

        let summary = DurationSummary::from_samples(&mut samples);
        let deltas_per_s = DELTAS as f64 / summary.median.as_secs_f64();
        let throughput_mib_s = output_bytes as f64 / summary.median.as_secs_f64() / 1024.0 / 1024.0;
        println!(
            "delta_writer_many_small_deltas deltas={DELTAS} bytes={output_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3} deltas_per_s={:.0} throughput_mib_s={:.1}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
            deltas_per_s,
            throughput_mib_s,
        );
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

    #[test]
    fn parses_cli_commands() {
        assert_eq!(parse_cli_command(vec![]).unwrap(), CliCommand::Interactive);
        assert_eq!(
            parse_cli_command(vec!["hello".to_string(), "world".to_string()]).unwrap(),
            CliCommand::Prompt("hello world".to_string())
        );
        assert_eq!(
            parse_cli_command(vec!["login".to_string()]).unwrap(),
            CliCommand::LoginDeviceCode
        );
        assert_eq!(
            parse_cli_command(vec!["login".to_string(), "--device-code".to_string()]).unwrap(),
            CliCommand::LoginDeviceCode
        );
        assert_eq!(
            parse_cli_command(vec!["login".to_string(), "status".to_string()]).unwrap(),
            CliCommand::LoginStatus
        );
        assert_eq!(
            parse_cli_command(vec!["login".to_string(), "--status".to_string()]).unwrap(),
            CliCommand::LoginStatus
        );
        assert_eq!(
            parse_cli_command(vec!["contract".to_string()]).unwrap(),
            CliCommand::Contract
        );
        assert_eq!(
            parse_cli_command(vec!["logout".to_string()]).unwrap(),
            CliCommand::Logout
        );
        assert!(parse_cli_command(vec!["logout".to_string(), "now".to_string()]).is_err());
        assert!(parse_cli_command(vec!["login".to_string(), "bad".to_string()]).is_err());
    }
}
