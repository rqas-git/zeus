//! Pi-style semantic context compaction.

use std::collections::BTreeSet;

use crate::agent_loop::AgentItem;
use crate::agent_loop::AgentMessage;
use crate::agent_loop::MessageId;
use crate::agent_loop::MessageRole;
use crate::config::CompactionConfig;

/// System prompt used for summary generation.
pub(crate) const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
// Pi truncates tool results to 2k characters before asking the model to summarize.
const TOOL_RESULT_MAX_CHARS: usize = 2_000;
// Pi uses a conservative chars/4 token estimate when exact usage is unavailable.
const APPROX_CHARS_PER_TOKEN: usize = 4;

const SUMMARY_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARY_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const TURN_PREFIX_SUMMARY_PROMPT: &str = r#"This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix."#;

/// File operation details persisted with compaction entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct CompactionDetails {
    pub(crate) read_files: Vec<String>,
    pub(crate) modified_files: Vec<String>,
}

/// A generated compaction checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactionResult {
    pub(crate) summary: String,
    pub(crate) first_kept_message_id: MessageId,
    pub(crate) tokens_before: u64,
    pub(crate) details: CompactionDetails,
}

/// Conversation slice selected for compaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactionPreparation {
    pub(crate) first_kept_message_id: MessageId,
    pub(crate) messages_to_summarize: Vec<AgentMessage>,
    pub(crate) turn_prefix_messages: Vec<AgentMessage>,
    pub(crate) is_split_turn: bool,
    pub(crate) tokens_before: u64,
    pub(crate) previous_summary: Option<String>,
    pub(crate) details: CompactionDetails,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CutPoint {
    first_kept_index: usize,
    turn_start_index: Option<usize>,
}

#[derive(Default)]
struct FileOperations {
    read: BTreeSet<String>,
    modified: BTreeSet<String>,
}

/// Prepares a pi-style compaction from linear session messages.
pub(crate) fn prepare_compaction(
    messages: &[AgentMessage],
    settings: CompactionConfig,
) -> Option<CompactionPreparation> {
    if messages
        .last()
        .is_some_and(|message| matches!(message.item(), AgentItem::Compaction { .. }))
    {
        return None;
    }

    let previous_compaction_index = latest_compaction_index(messages);
    let mut previous_summary = None;
    let mut boundary_start = 0;
    let mut file_ops = FileOperations::default();

    if let Some(index) = previous_compaction_index {
        if let AgentItem::Compaction {
            summary,
            first_kept_message_id,
            details,
            ..
        } = messages[index].item()
        {
            previous_summary = Some(summary.clone());
            boundary_start = messages
                .iter()
                .position(|message| message.id() == *first_kept_message_id)
                .unwrap_or(index + 1);
            merge_details(&mut file_ops, details);
        }
    }

    let boundary_end = messages.len();
    let cut = find_cut_point(
        messages,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens(),
    )?;
    let first_kept_message = messages.get(cut.first_kept_index)?;
    if matches!(first_kept_message.item(), AgentItem::Compaction { .. }) {
        return None;
    }

    let history_end = cut.turn_start_index.unwrap_or(cut.first_kept_index);
    let messages_to_summarize = visible_messages(messages, boundary_start, history_end);
    let turn_prefix_messages = cut
        .turn_start_index
        .map(|start| visible_messages(messages, start, cut.first_kept_index))
        .unwrap_or_default();

    for message in messages_to_summarize
        .iter()
        .chain(turn_prefix_messages.iter())
    {
        extract_file_operations(message, &mut file_ops);
    }

    Some(CompactionPreparation {
        first_kept_message_id: first_kept_message.id(),
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut.turn_start_index.is_some(),
        tokens_before: estimate_session_tokens(messages),
        previous_summary,
        details: details_from_ops(file_ops),
    })
}

/// Creates the user-visible summary message sent back into model context.
pub(crate) fn compaction_context_text(summary: &str) -> String {
    format!("{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}")
}

/// Creates the main summary prompt.
pub(crate) fn summary_prompt(
    preparation: &CompactionPreparation,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n",
        serialize_conversation(&preparation.messages_to_summarize)
    );
    if let Some(previous_summary) = &preparation.previous_summary {
        prompt.push_str("<previous-summary>\n");
        prompt.push_str(previous_summary);
        prompt.push_str("\n</previous-summary>\n\n");
        prompt.push_str(UPDATE_SUMMARY_PROMPT);
    } else {
        prompt.push_str(SUMMARY_PROMPT);
    }
    if let Some(custom_instructions) = custom_instructions.filter(|value| !value.trim().is_empty())
    {
        prompt.push_str("\n\nAdditional focus: ");
        prompt.push_str(custom_instructions.trim());
    }
    prompt
}

/// Creates the split-turn prefix summary prompt.
pub(crate) fn turn_prefix_prompt(preparation: &CompactionPreparation) -> String {
    format!(
        "<conversation>\n{}\n</conversation>\n\n{TURN_PREFIX_SUMMARY_PROMPT}",
        serialize_conversation(&preparation.turn_prefix_messages)
    )
}

/// Appends pi-style file operation sections to a generated summary.
pub(crate) fn with_file_operations(mut summary: String, details: &CompactionDetails) -> String {
    if !details.read_files.is_empty() {
        summary.push_str("\n\n<read-files>\n");
        summary.push_str(&details.read_files.join("\n"));
        summary.push_str("\n</read-files>");
    }
    if !details.modified_files.is_empty() {
        summary.push_str("\n\n<modified-files>\n");
        summary.push_str(&details.modified_files.join("\n"));
        summary.push_str("\n</modified-files>");
    }
    summary
}

/// Returns true when an error looks like a provider context overflow.
pub(crate) fn is_context_overflow_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    if message.contains("rate limit") || message.contains("too many requests") {
        return false;
    }

    [
        "prompt is too long",
        "request_too_large",
        "input is too long for requested model",
        "exceeds the context window",
        "input token count",
        "maximum prompt length",
        "reduce the length of the messages",
        "maximum context length",
        "exceeds the available context size",
        "greater than the context length",
        "context window exceeds limit",
        "exceeded model token limit",
        "context_length_exceeded",
        "context length exceeded",
        "too many tokens",
        "token limit exceeded",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

/// Estimates the active model context for threshold checks.
pub(crate) fn estimate_session_tokens(messages: &[AgentMessage]) -> u64 {
    let chars = if let Some(index) = latest_compaction_index(messages) {
        let AgentItem::Compaction {
            summary,
            first_kept_message_id,
            ..
        } = messages[index].item()
        else {
            unreachable!("latest compaction index must point at a compaction")
        };
        let summary_chars = compaction_context_text(summary).len();
        let first_kept = messages
            .iter()
            .take(index)
            .position(|message| message.id() == *first_kept_message_id)
            .unwrap_or(index);
        summary_chars
            + messages[first_kept..index]
                .iter()
                .map(estimate_message_chars)
                .sum::<usize>()
            + messages[index + 1..]
                .iter()
                .map(estimate_message_chars)
                .sum::<usize>()
    } else {
        messages.iter().map(estimate_message_chars).sum::<usize>()
    };
    approx_tokens(chars) as u64
}

fn latest_compaction_index(messages: &[AgentMessage]) -> Option<usize> {
    messages
        .iter()
        .rposition(|message| matches!(message.item(), AgentItem::Compaction { .. }))
}

fn visible_messages(messages: &[AgentMessage], start: usize, end: usize) -> Vec<AgentMessage> {
    messages[start..end]
        .iter()
        .filter(|message| !matches!(message.item(), AgentItem::Compaction { .. }))
        .cloned()
        .collect()
}

fn find_cut_point(
    messages: &[AgentMessage],
    start: usize,
    end: usize,
    keep_recent_tokens: u64,
) -> Option<CutPoint> {
    let cut_points = valid_cut_points(messages, start, end);
    if cut_points.is_empty() {
        return (start < end).then_some(CutPoint {
            first_kept_index: start,
            turn_start_index: None,
        });
    }

    let mut accumulated = 0u64;
    let mut cut_index = cut_points[0];
    for index in (start..end).rev() {
        let item = messages[index].item();
        if matches!(item, AgentItem::Compaction { .. }) {
            continue;
        }
        accumulated = accumulated.saturating_add(approx_tokens(estimate_item_chars(item)) as u64);
        if accumulated >= keep_recent_tokens {
            if let Some(point) = cut_points.iter().copied().find(|point| *point >= index) {
                cut_index = point;
            }
            break;
        }
    }

    let turn_start_index = if is_user_message(&messages[cut_index]) {
        None
    } else {
        find_turn_start_index(messages, cut_index, start)
    };
    Some(CutPoint {
        first_kept_index: cut_index,
        turn_start_index,
    })
}

fn valid_cut_points(messages: &[AgentMessage], start: usize, end: usize) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for index in start..end {
        match messages[index].item() {
            AgentItem::Message { .. } | AgentItem::FunctionCall { .. } => cut_points.push(index),
            AgentItem::FunctionOutput { .. } | AgentItem::Compaction { .. } => {}
        }
    }
    cut_points
}

fn find_turn_start_index(messages: &[AgentMessage], index: usize, start: usize) -> Option<usize> {
    (start..=index)
        .rev()
        .find(|index| is_user_message(&messages[*index]))
}

fn is_user_message(message: &AgentMessage) -> bool {
    matches!(
        message.item(),
        AgentItem::Message {
            role: MessageRole::User,
            ..
        }
    )
}

fn serialize_conversation(messages: &[AgentMessage]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match message.item() {
            AgentItem::Message {
                role: MessageRole::User,
                text,
            } => parts.push(format!("[User]: {text}")),
            AgentItem::Message {
                role: MessageRole::Assistant,
                text,
            } => parts.push(format!("[Assistant]: {text}")),
            AgentItem::FunctionCall {
                name, arguments, ..
            } => parts.push(format!("[Assistant tool calls]: {name}({arguments})")),
            AgentItem::FunctionOutput { output, .. } => {
                parts.push(format!(
                    "[Tool result]: {}",
                    truncate_for_summary(output, TOOL_RESULT_MAX_CHARS)
                ));
            }
            AgentItem::Compaction { .. } => {}
        }
    }
    parts.join("\n\n")
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_chars)
        .last()
        .unwrap_or(0);
    let truncated = text.len().saturating_sub(end);
    format!(
        "{}\n\n[... {truncated} more characters truncated]",
        &text[..end]
    )
}

fn estimate_message_chars(message: &AgentMessage) -> usize {
    estimate_item_chars(message.item())
}

fn estimate_item_chars(item: &AgentItem) -> usize {
    match item {
        AgentItem::Message { text, .. } => text.len(),
        AgentItem::FunctionCall {
            item_id,
            call_id,
            name,
            arguments,
        } => item_id.as_deref().map_or(0, str::len) + call_id.len() + name.len() + arguments.len(),
        AgentItem::FunctionOutput {
            call_id, output, ..
        } => call_id.len() + output.len(),
        AgentItem::Compaction { summary, .. } => summary.len(),
    }
}

fn approx_tokens(chars: usize) -> usize {
    chars.div_ceil(APPROX_CHARS_PER_TOKEN)
}

fn merge_details(file_ops: &mut FileOperations, details: &CompactionDetails) {
    file_ops.read.extend(details.read_files.iter().cloned());
    file_ops
        .modified
        .extend(details.modified_files.iter().cloned());
}

fn details_from_ops(file_ops: FileOperations) -> CompactionDetails {
    let modified = file_ops.modified;
    let read_files = file_ops
        .read
        .into_iter()
        .filter(|path| !modified.contains(path))
        .collect();
    CompactionDetails {
        read_files,
        modified_files: modified.into_iter().collect(),
    }
}

fn extract_file_operations(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentItem::FunctionCall {
        name, arguments, ..
    } = message.item()
    else {
        return;
    };
    match name.as_str() {
        "read_file" | "read_file_range" => {
            if let Some(path) = json_string_field(arguments, "path") {
                file_ops.read.insert(path);
            }
        }
        "apply_patch" => {
            if let Some(patch) = json_string_field(arguments, "patch") {
                extract_patch_paths(&patch, &mut file_ops.modified);
            }
        }
        _ => {}
    }
}

fn json_string_field(arguments: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()?
        .get(field)?
        .as_str()
        .map(ToString::to_string)
}

fn extract_patch_paths(patch: &str, modified: &mut BTreeSet<String>) {
    for line in patch.lines() {
        for prefix in [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ] {
            if let Some(path) = line.strip_prefix(prefix) {
                let path = path.trim();
                if !path.is_empty() {
                    modified.insert(path.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u64, role: MessageRole, text: &str) -> AgentMessage {
        AgentMessage::from_parts(
            MessageId::new(id),
            AgentItem::Message {
                role,
                text: text.to_string(),
            },
        )
    }

    fn tool_call(id: u64, name: &str, arguments: &str) -> AgentMessage {
        AgentMessage::from_parts(
            MessageId::new(id),
            AgentItem::FunctionCall {
                item_id: None,
                call_id: format!("call_{id}"),
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        )
    }

    #[test]
    fn prepares_initial_compaction_with_kept_tail() {
        let messages = vec![
            message(1, MessageRole::User, "first user"),
            message(2, MessageRole::Assistant, "first answer"),
            message(3, MessageRole::User, "second user"),
            message(4, MessageRole::Assistant, "second answer"),
        ];

        let preparation = prepare_compaction(&messages, CompactionConfig::for_test(100, 10, 3))
            .expect("session should compact");

        assert_eq!(preparation.first_kept_message_id, MessageId::new(4));
        assert_eq!(preparation.messages_to_summarize.len(), 2);
        assert!(preparation.is_split_turn);
        assert_eq!(preparation.turn_prefix_messages[0].id(), MessageId::new(3));
    }

    #[test]
    fn repeated_compaction_starts_from_previous_kept_boundary() {
        let previous_details = CompactionDetails {
            read_files: vec!["src/lib.rs".to_string()],
            modified_files: Vec::new(),
        };
        let messages = vec![
            message(1, MessageRole::User, "old"),
            message(2, MessageRole::Assistant, "old answer"),
            message(3, MessageRole::User, "kept before previous compaction"),
            AgentMessage::from_parts(
                MessageId::new(4),
                AgentItem::Compaction {
                    summary: "previous summary".to_string(),
                    first_kept_message_id: MessageId::new(3),
                    tokens_before: 20,
                    details: previous_details,
                },
            ),
            tool_call(5, "read_file", r#"{"path":"Cargo.toml"}"#),
            message(6, MessageRole::Assistant, "latest"),
        ];

        let preparation = prepare_compaction(&messages, CompactionConfig::for_test(100, 10, 2))
            .expect("session should compact again");

        assert_eq!(
            preparation.previous_summary.as_deref(),
            Some("previous summary")
        );
        assert!(preparation
            .messages_to_summarize
            .iter()
            .chain(preparation.turn_prefix_messages.iter())
            .any(|message| message.id() == MessageId::new(3)));
        assert!(preparation
            .details
            .read_files
            .contains(&"src/lib.rs".to_string()));
        assert!(preparation
            .details
            .read_files
            .contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn summary_prompt_updates_previous_summary_and_truncates_tool_output() {
        let messages = vec![AgentMessage::from_parts(
            MessageId::new(1),
            AgentItem::FunctionOutput {
                call_id: "call_1".to_string(),
                output: "x".repeat(TOOL_RESULT_MAX_CHARS + 8),
                success: true,
            },
        )];
        let preparation = CompactionPreparation {
            first_kept_message_id: MessageId::new(1),
            messages_to_summarize: messages,
            turn_prefix_messages: Vec::new(),
            is_split_turn: false,
            tokens_before: 10,
            previous_summary: Some("old summary".to_string()),
            details: CompactionDetails::default(),
        };

        let prompt = summary_prompt(&preparation, Some("focus paths"));

        assert!(prompt.contains("<previous-summary>\nold summary\n</previous-summary>"));
        assert!(prompt.contains("Additional focus: focus paths"));
        assert!(prompt.contains("more characters truncated"));
    }

    #[test]
    fn detects_context_overflow_errors_without_rate_limits() {
        assert!(is_context_overflow_error(
            "Your input exceeds the context window of this model"
        ));
        assert!(!is_context_overflow_error(
            "Rate limit: too many tokens per minute"
        ));
    }
}
