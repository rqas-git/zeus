//! Built-in tool registry and execution.

use std::collections::BinaryHeap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use fff_search::file_picker::FilePicker;
use fff_search::grep::parse_grep_query;
use fff_search::grep::GrepMode;
use fff_search::grep::GrepSearchOptions;
use fff_search::FFFMode;
use fff_search::FilePickerOptions;
use fff_search::FuzzySearchOptions;
use fff_search::MixedItemRef;
use fff_search::PaginationArgs;
use fff_search::QueryParser;
use fff_search::SharedFrecency;
use fff_search::SharedPicker;
use fff_search::SharedQueryTracker;
use serde::de::DeserializeOwned;
use serde::ser::SerializeMap;
use serde::ser::SerializeStruct;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncBufRead;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncSeekExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::agent_loop::ModelToolCall;
use crate::agent_loop::TurnCancellation;

const READ_FILE_TOOL: &str = "read_file";
const READ_FILE_RANGE_TOOL: &str = "read_file_range";
const LIST_DIR_TOOL: &str = "list_dir";
const SEARCH_FILES_TOOL: &str = "search_files";
const SEARCH_TEXT_TOOL: &str = "search_text";
const APPLY_PATCH_TOOL: &str = "apply_patch";
const EXEC_COMMAND_TOOL: &str = "exec_command";
const GIT_STATUS_TOOL: &str = "git_status";
const GIT_DIFF_TOOL: &str = "git_diff";
const GIT_LOG_TOOL: &str = "git_log";
const GIT_QUERY_TOOL: &str = "git_query";
const GIT_ADD_TOOL: &str = "git_add";
const GIT_RESTORE_TOOL: &str = "git_restore";
const GIT_COMMIT_TOOL: &str = "git_commit";
const MAX_FILE_BYTES: usize = 64 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n[truncated: file exceeds 65536 bytes]";
const RANGE_TRUNCATION_MARKER: &str = "\n[truncated: range exceeds requested max_bytes]";
const DEFAULT_READ_LINE_LIMIT: usize = 2000;
const MAX_READ_LINE_LIMIT: usize = 2000;
const MAX_READ_LINE_BYTES: usize = 2000;
const READ_LINE_TRUNCATION_SUFFIX: &str = "... (line truncated to 2000 bytes)";
const MAX_DIR_ENTRIES: usize = 200;
const DEFAULT_LIST_DIR_LIMIT: usize = MAX_DIR_ENTRIES;
const MAX_LIST_DIR_LIMIT: usize = 500;
const DEFAULT_LIST_DIR_DEPTH: usize = 1;
const MAX_LIST_DIR_DEPTH: usize = 4;
const MAX_LIST_DIR_PAGE_WINDOW: usize = 10_000;
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_PATCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 30_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 300_000;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_COMMAND_TOTAL_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const COMMAND_OUTPUT_DIR: &str = "target/rust-agent-tool-output";
const COMMAND_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_GIT_PATHS: usize = 128;
const MAX_GIT_PATH_BYTES: usize = 8 * 1024;
const MAX_GIT_COMMIT_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_GIT_QUERY_ARGS: usize = 64;
const MAX_GIT_QUERY_ARG_BYTES: usize = 8 * 1024;
const DEFAULT_GIT_LOG_COUNT: usize = 10;
const MAX_GIT_LOG_COUNT: usize = 50;
const DEFAULT_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_OFFSET: usize = 100_000;
const MAX_SEARCH_TEXT_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const DEFAULT_FFF_SEARCH_CONCURRENCY: usize = 1;
pub(crate) const MAX_FFF_SEARCH_CONCURRENCY: usize = 16;
const DEFAULT_TEXT_SEARCH_TIMEOUT_MS: u64 = 250;
const MAX_TEXT_CONTEXT_LINES: usize = 3;
const FFF_SCAN_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);

static COMMAND_OUTPUT_ARTIFACT_COUNTER: AtomicUsize = AtomicUsize::new(0);

const READ_FILE_TOOL_SPEC: ToolSpec = ToolSpec {
    name: READ_FILE_TOOL,
    description: "Read a UTF-8 text file from the current workspace, optionally by line range.",
    parameters: ToolParameters::ReadFile,
    supports_parallel: true,
};
const READ_FILE_RANGE_TOOL_SPEC: ToolSpec = ToolSpec {
    name: READ_FILE_RANGE_TOOL,
    description: "Read a byte range from a UTF-8 text file in the current workspace.",
    parameters: ToolParameters::ReadFileRange,
    supports_parallel: true,
};
const LIST_DIR_TOOL_SPEC: ToolSpec = ToolSpec {
    name: LIST_DIR_TOOL,
    description: "List workspace directory entries with pagination and optional depth.",
    parameters: ToolParameters::ListDir,
    supports_parallel: true,
};
const SEARCH_FILES_TOOL_SPEC: ToolSpec = ToolSpec {
    name: SEARCH_FILES_TOOL,
    description: "Fuzzy-search indexed workspace files and directories by path.",
    parameters: ToolParameters::SearchFiles,
    supports_parallel: false,
};
const SEARCH_TEXT_TOOL_SPEC: ToolSpec = ToolSpec {
    name: SEARCH_TEXT_TOOL,
    description: "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
    parameters: ToolParameters::SearchText,
    supports_parallel: false,
};
const APPLY_PATCH_TOOL_SPEC: ToolSpec = ToolSpec {
    name: APPLY_PATCH_TOOL,
    description: "Apply a workspace-confined patch that adds, updates, or deletes UTF-8 files.",
    parameters: ToolParameters::ApplyPatch,
    supports_parallel: false,
};
const EXEC_COMMAND_TOOL_SPEC: ToolSpec = ToolSpec {
    name: EXEC_COMMAND_TOOL,
    description: "Execute a bash command from the workspace and return bounded output.",
    parameters: ToolParameters::ExecCommand,
    supports_parallel: false,
};
const GIT_STATUS_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_STATUS_TOOL,
    description: "Show concise git worktree status for the workspace repository.",
    parameters: ToolParameters::NoArgs,
    supports_parallel: false,
};
const GIT_DIFF_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_DIFF_TOOL,
    description: "Show bounded git diff output for unstaged or staged workspace changes.",
    parameters: ToolParameters::GitDiff,
    supports_parallel: false,
};
const GIT_LOG_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_LOG_TOOL,
    description: "Show recent git commits for the workspace repository.",
    parameters: ToolParameters::GitLog,
    supports_parallel: false,
};
const GIT_QUERY_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_QUERY_TOOL,
    description: "Run an allowlisted read-only git query for workspace inspection.",
    parameters: ToolParameters::GitQuery,
    supports_parallel: false,
};
const GIT_ADD_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_ADD_TOOL,
    description: "Stage explicit workspace-relative paths for the next git commit.",
    parameters: ToolParameters::GitAdd,
    supports_parallel: false,
};
const GIT_RESTORE_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_RESTORE_TOOL,
    description: "Restore or unstage explicit workspace-relative paths.",
    parameters: ToolParameters::GitRestore,
    supports_parallel: false,
};
const GIT_COMMIT_TOOL_SPEC: ToolSpec = ToolSpec {
    name: GIT_COMMIT_TOOL,
    description: "Create an atomic git commit for explicit workspace-relative paths.",
    parameters: ToolParameters::GitCommit,
    supports_parallel: false,
};

const TOOL_DEFINITIONS: &[ToolDefinition] = &[
    ToolDefinition::new(ToolPolicy::ReadOnly, READ_FILE_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::ReadOnly, READ_FILE_RANGE_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::ReadOnly, LIST_DIR_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::ReadOnly, SEARCH_FILES_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::ReadOnly, SEARCH_TEXT_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceWrite, APPLY_PATCH_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, EXEC_COMMAND_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_STATUS_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_DIFF_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_LOG_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_QUERY_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_ADD_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_RESTORE_TOOL_SPEC),
    ToolDefinition::new(ToolPolicy::WorkspaceExec, GIT_COMMIT_TOOL_SPEC),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToolDefinition {
    min_policy: ToolPolicy,
    spec: ToolSpec,
}

impl ToolDefinition {
    const fn new(min_policy: ToolPolicy, spec: ToolSpec) -> Self {
        Self { min_policy, spec }
    }
}

/// Permission set controlling which built-in tools are exposed and executable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToolPolicy {
    #[default]
    ReadOnly,
    WorkspaceWrite,
    WorkspaceExec,
}

impl ToolPolicy {
    const fn allows(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::WorkspaceWrite => 1,
            Self::WorkspaceExec => 2,
        }
    }
}

fn tool_specs_for_policy(policy: ToolPolicy) -> Vec<ToolSpec> {
    TOOL_DEFINITIONS
        .iter()
        .filter(|definition| policy.allows(definition.min_policy))
        .map(|definition| definition.spec)
        .collect()
}

/// Model-visible tool declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolSpec {
    name: &'static str,
    description: &'static str,
    parameters: ToolParameters,
    supports_parallel: bool,
}

impl ToolSpec {
    /// Returns the model-facing tool name.
    pub(crate) const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the model-facing description.
    pub(crate) const fn description(self) -> &'static str {
        self.description
    }

    /// Returns a compact stable representation of the parameter schema.
    pub(crate) const fn parameters_cache_key(self) -> &'static str {
        self.parameters.cache_key()
    }

    /// Returns `true` when this tool can safely execute with other tools.
    pub(crate) const fn supports_parallel(self) -> bool {
        self.supports_parallel
    }
}

impl Serialize for ToolSpec {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ToolSpec", 4)?;
        state.serialize_field("type", "function")?;
        state.serialize_field("name", self.name)?;
        state.serialize_field("description", self.description)?;
        state.serialize_field("parameters", &self.parameters)?;
        state.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolParameters {
    ReadFile,
    ReadFileRange,
    ListDir,
    SearchFiles,
    SearchText,
    ApplyPatch,
    ExecCommand,
    NoArgs,
    GitDiff,
    GitLog,
    GitQuery,
    GitAdd,
    GitRestore,
    GitCommit,
}

impl ToolParameters {
    const fn cache_key(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file:path:string:required:offset:integer:limit:integer",
            Self::ReadFileRange => {
                "read_file_range:path:string:required:offset:integer:max_bytes:integer"
            }
            Self::ListDir => "list_dir:path:string:required:offset:integer:limit:integer:depth:integer",
            Self::SearchFiles => "search_files:query:string:required:limit:integer:offset:integer",
            Self::SearchText => {
                "search_text:query:string:required:mode:string:limit:integer:file_offset:integer:before_context:integer:after_context:integer"
            }
            Self::ApplyPatch => "apply_patch:patch:string:required:no_additional_properties",
            Self::ExecCommand => {
                "exec_command:command:string:required:cwd:string:timeout_ms:integer:max_output_bytes:integer"
            }
            Self::NoArgs => "no_args:no_additional_properties",
            Self::GitDiff => "git_diff:staged:boolean:path:string:max_output_bytes:integer",
            Self::GitLog => "git_log:max_count:integer",
            Self::GitQuery => {
                "git_query:command:string:required:args:string_array:max_output_bytes:integer"
            }
            Self::GitAdd => "git_add:paths:string_array:required",
            Self::GitRestore => "git_restore:paths:string_array:required:staged:boolean",
            Self::GitCommit => "git_commit:message:string:required:paths:string_array:required",
        }
    }
}

impl Serialize for ToolParameters {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::ReadFile => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &ReadFileProperties)?;
                map.serialize_entry("required", &["path"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::ReadFileRange => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &ReadFileRangeProperties)?;
                map.serialize_entry("required", &["path"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::ListDir => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &ListDirProperties)?;
                map.serialize_entry("required", &["path"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::SearchFiles => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &SearchFilesProperties)?;
                map.serialize_entry("required", &["query"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::SearchText => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &SearchTextProperties)?;
                map.serialize_entry("required", &["query"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::ApplyPatch => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &ApplyPatchProperties)?;
                map.serialize_entry("required", &["patch"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::ExecCommand => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &ExecCommandProperties)?;
                map.serialize_entry("required", &["command"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::NoArgs => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &EmptyProperties)?;
                map.serialize_entry("required", &[] as &[&str])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitDiff => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitDiffProperties)?;
                map.serialize_entry("required", &[] as &[&str])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitLog => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitLogProperties)?;
                map.serialize_entry("required", &[] as &[&str])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitQuery => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitQueryProperties)?;
                map.serialize_entry("required", &["command"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitAdd => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitAddProperties)?;
                map.serialize_entry("required", &["paths"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitRestore => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitRestoreProperties)?;
                map.serialize_entry("required", &["paths"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
            Self::GitCommit => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &GitCommitProperties)?;
                map.serialize_entry("required", &["message", "paths"])?;
                map.serialize_entry("additionalProperties", &false)?;
                map.end()
            }
        }
    }
}

struct StringProperty {
    description: &'static str,
}

impl Serialize for StringProperty {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("StringProperty", 2)?;
        state.serialize_field("type", "string")?;
        state.serialize_field("description", self.description)?;
        state.end()
    }
}

struct IntegerProperty {
    description: &'static str,
    minimum: usize,
    maximum: usize,
}

impl Serialize for IntegerProperty {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("IntegerProperty", 4)?;
        state.serialize_field("type", "integer")?;
        state.serialize_field("description", self.description)?;
        state.serialize_field("minimum", &self.minimum)?;
        state.serialize_field("maximum", &self.maximum)?;
        state.end()
    }
}

struct BooleanProperty {
    description: &'static str,
}

impl Serialize for BooleanProperty {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("BooleanProperty", 2)?;
        state.serialize_field("type", "boolean")?;
        state.serialize_field("description", self.description)?;
        state.end()
    }
}

struct StringArrayProperty {
    description: &'static str,
}

impl Serialize for StringArrayProperty {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("StringArrayProperty", 3)?;
        state.serialize_field("type", "array")?;
        state.serialize_field("description", self.description)?;
        state.serialize_field(
            "items",
            &StringItems {
                item_type: "string",
            },
        )?;
        state.end()
    }
}

struct StringItems {
    item_type: &'static str,
}

impl Serialize for StringItems {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("StringItems", 1)?;
        state.serialize_field("type", self.item_type)?;
        state.end()
    }
}

struct ReadFileProperties;

impl Serialize for ReadFileProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "path",
            &StringProperty {
                description: "Workspace-relative path to the file to read.",
            },
        )?;
        map.serialize_entry(
            "offset",
            &IntegerProperty {
                description: "1-indexed line number to start reading from.",
                minimum: 1,
                maximum: usize::MAX,
            },
        )?;
        map.serialize_entry(
            "limit",
            &IntegerProperty {
                description: "Maximum number of lines to read.",
                minimum: 1,
                maximum: MAX_READ_LINE_LIMIT,
            },
        )?;
        map.end()
    }
}

struct ListDirProperties;

impl Serialize for ListDirProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry(
            "path",
            &StringProperty {
                description: "Workspace-relative directory path. Use . for the workspace root.",
            },
        )?;
        map.serialize_entry(
            "offset",
            &IntegerProperty {
                description: "1-indexed entry number to start listing from.",
                minimum: 1,
                maximum: MAX_LIST_DIR_PAGE_WINDOW,
            },
        )?;
        map.serialize_entry(
            "limit",
            &IntegerProperty {
                description: "Maximum number of entries to return.",
                minimum: 1,
                maximum: MAX_LIST_DIR_LIMIT,
            },
        )?;
        map.serialize_entry(
            "depth",
            &IntegerProperty {
                description: "Maximum directory depth to traverse.",
                minimum: 1,
                maximum: MAX_LIST_DIR_DEPTH,
            },
        )?;
        map.end()
    }
}

struct SearchFilesProperties;

impl Serialize for SearchFilesProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "query",
            &StringProperty {
                description: "Fuzzy path query, such as main rs, src tools, or config.",
            },
        )?;
        map.serialize_entry(
            "limit",
            &IntegerProperty {
                description: "Maximum number of results to return.",
                minimum: 1,
                maximum: MAX_SEARCH_RESULTS,
            },
        )?;
        map.serialize_entry(
            "offset",
            &IntegerProperty {
                description: "Result offset for pagination.",
                minimum: 0,
                maximum: MAX_SEARCH_OFFSET,
            },
        )?;
        map.end()
    }
}

struct SearchTextProperties;

impl Serialize for SearchTextProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry(
            "query",
            &StringProperty {
                description: "Text query. Path constraints like src/*.rs may be included.",
            },
        )?;
        map.serialize_entry(
            "mode",
            &StringProperty {
                description: "Search mode: plain, regex, or fuzzy. Defaults to plain.",
            },
        )?;
        map.serialize_entry(
            "limit",
            &IntegerProperty {
                description: "Maximum number of matching lines to return.",
                minimum: 1,
                maximum: MAX_SEARCH_RESULTS,
            },
        )?;
        map.serialize_entry(
            "file_offset",
            &IntegerProperty {
                description: "File pagination offset returned by an earlier search_text call.",
                minimum: 0,
                maximum: MAX_SEARCH_OFFSET,
            },
        )?;
        map.serialize_entry(
            "before_context",
            &IntegerProperty {
                description: "Context lines to include before each match.",
                minimum: 0,
                maximum: MAX_TEXT_CONTEXT_LINES,
            },
        )?;
        map.serialize_entry(
            "after_context",
            &IntegerProperty {
                description: "Context lines to include after each match.",
                minimum: 0,
                maximum: MAX_TEXT_CONTEXT_LINES,
            },
        )?;
        map.end()
    }
}

struct ApplyPatchProperties;

impl Serialize for ApplyPatchProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            "patch",
            &StringProperty {
                description: "Patch text using *** Begin Patch / *** End Patch blocks.",
            },
        )?;
        map.end()
    }
}

struct ReadFileRangeProperties;

impl Serialize for ReadFileRangeProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "path",
            &StringProperty {
                description: "Workspace-relative path to the file to read.",
            },
        )?;
        map.serialize_entry(
            "offset",
            &IntegerProperty {
                description: "Byte offset where reading starts. Defaults to 0.",
                minimum: 0,
                maximum: usize::MAX,
            },
        )?;
        map.serialize_entry(
            "max_bytes",
            &IntegerProperty {
                description: "Maximum bytes to return from the requested offset.",
                minimum: 1,
                maximum: MAX_FILE_BYTES,
            },
        )?;
        map.end()
    }
}

struct EmptyProperties;

impl Serialize for EmptyProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_map(Some(0))?.end()
    }
}

struct ExecCommandProperties;

impl Serialize for ExecCommandProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry(
            "command",
            &StringProperty {
                description: "Shell command to execute through bash.",
            },
        )?;
        map.serialize_entry(
            "cwd",
            &StringProperty {
                description: "Workspace-relative directory to run from. Defaults to .",
            },
        )?;
        map.serialize_entry(
            "timeout_ms",
            &IntegerProperty {
                description: "Maximum command runtime in milliseconds.",
                minimum: 1,
                maximum: MAX_COMMAND_TIMEOUT_MS as usize,
            },
        )?;
        map.serialize_entry(
            "max_output_bytes",
            &IntegerProperty {
                description: "Maximum stdout bytes and stderr bytes to retain separately.",
                minimum: 1,
                maximum: MAX_COMMAND_OUTPUT_BYTES,
            },
        )?;
        map.end()
    }
}

struct GitDiffProperties;

impl Serialize for GitDiffProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "staged",
            &BooleanProperty {
                description: "When true, show staged changes instead of unstaged changes.",
            },
        )?;
        map.serialize_entry(
            "path",
            &StringProperty {
                description: "Optional workspace-relative path to limit the diff.",
            },
        )?;
        map.serialize_entry(
            "max_output_bytes",
            &IntegerProperty {
                description: "Maximum stdout bytes and stderr bytes to retain separately.",
                minimum: 1,
                maximum: MAX_COMMAND_OUTPUT_BYTES,
            },
        )?;
        map.end()
    }
}

struct GitLogProperties;

impl Serialize for GitLogProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            "max_count",
            &IntegerProperty {
                description: "Maximum number of recent commits to return.",
                minimum: 1,
                maximum: MAX_GIT_LOG_COUNT,
            },
        )?;
        map.end()
    }
}

struct GitQueryProperties;

impl Serialize for GitQueryProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(
            "command",
            &StringProperty {
                description:
                    "Read-only git command: status, diff, log, show, blame, grep, ls-files, branch, rev-parse, merge-base, describe, worktree, or submodule.",
            },
        )?;
        map.serialize_entry(
            "args",
            &StringArrayProperty {
                description: "Arguments for the selected read-only git command.",
            },
        )?;
        map.serialize_entry(
            "max_output_bytes",
            &IntegerProperty {
                description: "Maximum stdout bytes and stderr bytes to retain separately.",
                minimum: 1,
                maximum: MAX_COMMAND_OUTPUT_BYTES,
            },
        )?;
        map.end()
    }
}

struct GitAddProperties;

impl Serialize for GitAddProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            "paths",
            &StringArrayProperty {
                description: "Workspace-relative paths to stage.",
            },
        )?;
        map.end()
    }
}

struct GitRestoreProperties;

impl Serialize for GitRestoreProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(
            "paths",
            &StringArrayProperty {
                description: "Workspace-relative paths to restore or unstage.",
            },
        )?;
        map.serialize_entry(
            "staged",
            &BooleanProperty {
                description:
                    "When true, unstage paths. When false or omitted, discard worktree changes for paths.",
            },
        )?;
        map.end()
    }
}

struct GitCommitProperties;

impl Serialize for GitCommitProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry(
            "message",
            &StringProperty {
                description: "Concise commit message.",
            },
        )?;
        map.serialize_entry(
            "paths",
            &StringArrayProperty {
                description: "Workspace-relative paths to include in this atomic commit.",
            },
        )?;
        map.end()
    }
}

/// Registry for built-in tools.
#[derive(Clone, Debug)]
pub(crate) struct ToolRegistry {
    specs: Vec<ToolSpec>,
    root: Arc<PathBuf>,
    search: FffSearchIndex,
    search_permits: Arc<Semaphore>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::for_root(root)
    }
}

impl ToolRegistry {
    /// Creates a tool registry for the current directory with explicit permissions and search concurrency.
    pub(crate) fn with_policy_and_search_concurrency(
        policy: ToolPolicy,
        search_concurrency: usize,
    ) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::for_root_with_policy_and_search_concurrency(root, policy, search_concurrency)
    }

    /// Creates a tool registry rooted at `root`.
    pub(crate) fn for_root(root: impl Into<PathBuf>) -> Self {
        Self::for_root_with_policy(root, ToolPolicy::ReadOnly)
    }

    /// Creates a tool registry rooted at `root` with explicit permissions.
    pub(crate) fn for_root_with_policy(root: impl Into<PathBuf>, policy: ToolPolicy) -> Self {
        Self::for_root_with_policy_and_search_concurrency(
            root,
            policy,
            DEFAULT_FFF_SEARCH_CONCURRENCY,
        )
    }

    /// Creates a tool registry rooted at `root` with explicit permissions and search concurrency.
    pub(crate) fn for_root_with_policy_and_search_concurrency(
        root: impl Into<PathBuf>,
        policy: ToolPolicy,
        search_concurrency: usize,
    ) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        let search_concurrency = search_concurrency.clamp(1, MAX_FFF_SEARCH_CONCURRENCY);
        Self {
            specs: tool_specs_for_policy(policy),
            search: FffSearchIndex::new(root.clone()),
            search_permits: Arc::new(Semaphore::new(search_concurrency)),
            root: Arc::new(root),
        }
    }

    /// Initializes the FFF search index on a blocking worker.
    pub(crate) fn spawn_search_index_warmup(&self) -> tokio::task::JoinHandle<Result<()>> {
        let search = self.search.clone();
        tokio::task::spawn_blocking(move || search.warm())
    }

    /// Returns the stable model-visible tool specs.
    pub(crate) fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    /// Returns `true` when every named tool can execute in parallel.
    pub(crate) fn supports_parallel(&self, name: &str) -> bool {
        self.specs()
            .iter()
            .find(|spec| spec.name() == name)
            .is_some_and(|spec| spec.supports_parallel())
    }

    /// Executes a model tool call and converts failures into model-visible output.
    #[cfg(test)]
    pub(crate) async fn execute(&self, call: ModelToolCall) -> ToolExecution {
        self.execute_ref(&call).await
    }

    /// Executes a borrowed model tool call and converts failures into model-visible output.
    #[cfg(test)]
    pub(crate) async fn execute_ref(&self, call: &ModelToolCall) -> ToolExecution {
        self.execute_ref_with_cancellation(call, &TurnCancellation::new())
            .await
    }

    /// Executes a borrowed model tool call with a shared cancellation signal.
    pub(crate) async fn execute_ref_with_cancellation(
        &self,
        call: &ModelToolCall,
        cancellation: &TurnCancellation,
    ) -> ToolExecution {
        if !self.specs().iter().any(|spec| spec.name() == call.name) {
            return ToolExecution {
                call_id: call.call_id.clone(),
                tool_name: call.name.clone(),
                output: "Tool error: tool is not enabled by the current policy".to_string(),
                success: false,
            };
        }

        match call.name.as_str() {
            EXEC_COMMAND_TOOL => {
                return exec_command(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_STATUS_TOOL => {
                return git_status(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_DIFF_TOOL => {
                return git_diff(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_LOG_TOOL => {
                return git_log(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_QUERY_TOOL => {
                return git_query(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_ADD_TOOL => {
                return git_add(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_RESTORE_TOOL => {
                return git_restore(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            GIT_COMMIT_TOOL => {
                return git_commit(&self.root, &call.arguments, cancellation)
                    .await
                    .map(|output| tool_execution(call, output.output, output.success))
                    .unwrap_or_else(|error| {
                        tool_execution(call, format!("Tool error: {error}"), false)
                    });
            }
            _ => {}
        }

        let result = match call.name.as_str() {
            READ_FILE_TOOL => read_file(&self.root, &call.arguments).await,
            READ_FILE_RANGE_TOOL => read_file_range(&self.root, &call.arguments).await,
            LIST_DIR_TOOL => list_dir(&self.root, &call.arguments).await,
            SEARCH_FILES_TOOL => {
                let search = self.search.clone();
                let search_permits = self.search_permits.clone();
                let arguments = call.arguments.clone();
                execute_blocking_search(
                    search_permits,
                    move || search.search_files(&arguments),
                    "search_files task failed",
                )
                .await
            }
            SEARCH_TEXT_TOOL => {
                let search = self.search.clone();
                let search_permits = self.search_permits.clone();
                let arguments = call.arguments.clone();
                execute_blocking_search(
                    search_permits,
                    move || search.search_text(&arguments),
                    "search_text task failed",
                )
                .await
            }
            APPLY_PATCH_TOOL => apply_patch(&self.root, &call.arguments).await,
            _ => Err(anyhow::anyhow!("unknown tool {}", call.name)),
        };

        match result {
            Ok(output) => tool_execution(call, output, true),
            Err(error) => tool_execution(call, format!("Tool error: {error}"), false),
        }
    }
}

/// Final result for one executed tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolExecution {
    pub(crate) call_id: String,
    pub(crate) tool_name: String,
    pub(crate) output: String,
    pub(crate) success: bool,
}

#[derive(Debug)]
struct ToolOutput {
    output: String,
    success: bool,
}

fn tool_execution(call: &ModelToolCall, output: String, success: bool) -> ToolExecution {
    ToolExecution {
        call_id: call.call_id.clone(),
        tool_name: call.name.clone(),
        output,
        success,
    }
}

async fn execute_blocking_search<F>(
    search_permits: Arc<Semaphore>,
    search: F,
    task_context: &'static str,
) -> Result<String>
where
    F: FnOnce() -> Result<String> + Send + 'static,
{
    let _permit = search_permits
        .acquire_owned()
        .await
        .context("search concurrency limiter was closed")?;
    tokio::task::spawn_blocking(search)
        .await
        .context(task_context)
        .and_then(|result| result)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArguments {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirArguments {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileRangeArguments {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchFilesArguments {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchTextArguments {
    query: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    file_offset: Option<usize>,
    #[serde(default)]
    before_context: Option<usize>,
    #[serde(default)]
    after_context: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArguments {
    patch: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecCommandArguments {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitStatusArguments {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitDiffArguments {
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitLogArguments {
    #[serde(default)]
    max_count: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitQueryArguments {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitAddArguments {
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitRestoreArguments {
    paths: Vec<String>,
    #[serde(default)]
    staged: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitCommitArguments {
    message: String,
    paths: Vec<String>,
}

#[derive(Clone, Debug)]
struct FffSearchIndex {
    root: Arc<PathBuf>,
    state: Arc<Mutex<Option<FffSearchState>>>,
}

impl FffSearchIndex {
    fn new(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            state: Arc::new(Mutex::new(None)),
        }
    }

    fn warm(&self) -> Result<()> {
        let _state = self.state()?;
        Ok(())
    }

    fn search_files(&self, arguments: &str) -> Result<String> {
        let args = parse_search_files_arguments(arguments)?;
        let query = trimmed_required("query", &args.query)?;
        let limit = bounded_limit(args.limit, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS);
        let offset = args.offset.unwrap_or(0).min(MAX_SEARCH_OFFSET);
        let state = self.ready_state()?;
        let picker_guard = state.picker.read()?;
        let picker = picker_guard
            .as_ref()
            .context("search index is not initialized")?;
        let tracker_guard = state.query_tracker.read()?;
        let parser = QueryParser::default();
        let query = parser.parse(query);
        let results = picker.fuzzy_search_mixed(
            &query,
            tracker_guard.as_ref(),
            FuzzySearchOptions {
                max_threads: 0,
                current_file: None,
                project_path: Some(&self.root),
                pagination: PaginationArgs { offset, limit },
                ..Default::default()
            },
        );

        Ok(format_file_search_results(picker, &results, offset, limit))
    }

    fn search_text(&self, arguments: &str) -> Result<String> {
        let args = parse_search_text_arguments(arguments)?;
        let query = trimmed_required("query", &args.query)?;
        let limit = bounded_limit(args.limit, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS);
        let state = self.ready_state()?;
        let picker_guard = state.picker.read()?;
        let picker = picker_guard
            .as_ref()
            .context("search index is not initialized")?;
        let query = parse_grep_query(query);
        let results = picker.grep(
            &query,
            &GrepSearchOptions {
                mode: parse_grep_mode(args.mode.as_deref())?,
                page_limit: limit,
                file_offset: args.file_offset.unwrap_or(0).min(MAX_SEARCH_OFFSET),
                before_context: args.before_context.unwrap_or(0).min(MAX_TEXT_CONTEXT_LINES),
                after_context: args.after_context.unwrap_or(0).min(MAX_TEXT_CONTEXT_LINES),
                time_budget_ms: DEFAULT_TEXT_SEARCH_TIMEOUT_MS,
                trim_whitespace: false,
                ..Default::default()
            },
        );

        Ok(format_text_search_results(picker, &results))
    }

    fn ready_state(&self) -> Result<FffSearchState> {
        let state = self.state()?;
        state.wait_until_scanned();
        Ok(state)
    }

    fn state(&self) -> Result<FffSearchState> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("search index lock was poisoned"))?;
        if let Some(state) = state.as_ref() {
            return Ok(state.clone());
        }

        let initialized = FffSearchState::new(&self.root)?;
        *state = Some(initialized.clone());
        Ok(initialized)
    }
}

#[derive(Clone, Debug)]
struct FffSearchState {
    picker: SharedPicker,
    query_tracker: SharedQueryTracker,
    _frecency: SharedFrecency,
}

impl FffSearchState {
    fn new(root: &Path) -> Result<Self> {
        let picker = SharedPicker::default();
        let frecency = SharedFrecency::noop();
        let query_tracker = SharedQueryTracker::noop();
        FilePicker::new_with_shared_state(
            picker.clone(),
            frecency.clone(),
            FilePickerOptions {
                base_path: root.to_string_lossy().into_owned(),
                mode: FFFMode::Ai,
                watch: true,
                ..Default::default()
            },
        )
        .context("failed to initialize FFF search index")?;
        Ok(Self {
            picker,
            query_tracker,
            _frecency: frecency,
        })
    }

    fn wait_until_scanned(&self) {
        while !self.picker.wait_for_scan(FFF_SCAN_WAIT_POLL_INTERVAL) {}
    }
}

async fn read_file(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_read_file_arguments(arguments)?;
    if args.offset.is_some() || args.limit.is_some() {
        return read_file_lines(root, &args).await;
    }

    let path = resolve_workspace_path(root, &args.path)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, &path)))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a file",
        display_path(root, &path)
    );

    let file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?;
    let mut bytes = Vec::with_capacity(MAX_FILE_BYTES + 1);
    let mut limited = file.take((MAX_FILE_BYTES + 1) as u64);
    limited
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?;
    let truncated = bytes.len() > MAX_FILE_BYTES;
    if truncated {
        bytes.truncate(MAX_FILE_BYTES);
    }

    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str(FILE_TRUNCATION_MARKER);
    }
    Ok(text)
}

async fn read_file_lines(root: &Path, args: &ReadFileArguments) -> Result<String> {
    let path = resolve_workspace_path(root, &args.path)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, &path)))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a file",
        display_path(root, &path)
    );

    let offset = args.offset.unwrap_or(1);
    anyhow::ensure!(offset > 0, "offset must be a 1-indexed line number");
    let limit = bounded_limit(args.limit, DEFAULT_READ_LINE_LIMIT, MAX_READ_LINE_LIMIT);
    let file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?;
    let mut reader = BufReader::new(file);
    let start = offset - 1;
    let mut raw = Vec::new();
    let mut retained_bytes = 0usize;
    let mut line_count = 0usize;
    let mut cut_by_bytes = false;
    let mut has_more = false;

    for _ in 0..start {
        let Some(_) = read_bounded_line(&mut reader, 0)
            .await
            .with_context(|| format!("failed to read {}", display_path(root, &path)))?
        else {
            anyhow::bail!("offset exceeds file length");
        };
        line_count = line_count.saturating_add(1);
    }

    while raw.len() < limit {
        let Some(line) = read_bounded_line(&mut reader, MAX_READ_LINE_BYTES)
            .await
            .with_context(|| format!("failed to read {}", display_path(root, &path)))?
        else {
            break;
        };
        line_count = line_count.saturating_add(1);

        let line = line.into_string();
        let line_bytes = line.len() + usize::from(!raw.is_empty());
        if retained_bytes.saturating_add(line_bytes) > MAX_FILE_BYTES {
            cut_by_bytes = true;
            has_more = true;
            break;
        }

        retained_bytes = retained_bytes.saturating_add(line_bytes);
        raw.push(line);
    }

    if raw.len() == limit && !cut_by_bytes {
        has_more = !reader
            .fill_buf()
            .await
            .with_context(|| format!("failed to read {}", display_path(root, &path)))?
            .is_empty();
    }

    if raw.is_empty() && offset > line_count.saturating_add(usize::from(line_count == 0)) {
        anyhow::bail!("offset exceeds file length");
    }

    Ok(format_line_read_result(
        root,
        &path,
        offset,
        line_count,
        &raw,
        has_more,
        cut_by_bytes,
    ))
}

async fn read_file_range(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_read_file_range_arguments(arguments)?;
    let path = resolve_workspace_path(root, &args.path)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, &path)))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a file",
        display_path(root, &path)
    );

    let offset = args.offset.unwrap_or(0);
    let max_bytes = bounded_limit(args.max_bytes, MAX_FILE_BYTES, MAX_FILE_BYTES);
    if offset >= metadata.len() {
        return Ok(String::new());
    }

    let file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?;
    let mut file = file;
    file.seek(SeekFrom::Start(offset))
        .await
        .with_context(|| format!("failed to seek {}", display_path(root, &path)))?;
    let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
    let mut limited = file.take((max_bytes + 1) as u64);
    limited
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?;
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
    }

    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str(RANGE_TRUNCATION_MARKER);
    }
    Ok(text)
}

async fn list_dir(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_list_dir_arguments(arguments)?;
    let path = resolve_workspace_path(root, &args.path)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, &path)))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "{} is not a directory",
        display_path(root, &path)
    );

    let offset = args.offset.unwrap_or(1);
    anyhow::ensure!(offset > 0, "offset must be a 1-indexed entry number");
    let limit = bounded_limit(args.limit, DEFAULT_LIST_DIR_LIMIT, MAX_LIST_DIR_LIMIT);
    let depth = bounded_limit(args.depth, DEFAULT_LIST_DIR_DEPTH, MAX_LIST_DIR_DEPTH);
    let desired_end = list_dir_page_end(offset, limit)?;
    if depth > 1 {
        return list_dir_deep(root, &path, offset, limit, desired_end, depth).await;
    }

    let mut entries = tokio::fs::read_dir(&path)
        .await
        .with_context(|| format!("failed to list {}", display_path(root, &path)))?;
    let mut names = BinaryHeap::new();
    let mut total_entries = 0usize;

    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("failed to read {}", display_path(root, &path)))?
    {
        let file_type = entry.file_type().await.with_context(|| {
            format!("failed to inspect {}", entry.file_name().to_string_lossy())
        })?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            name.push('/');
        }
        total_entries = total_entries.saturating_add(1);
        if names.len() < desired_end {
            names.push(name);
        } else if names.peek().is_some_and(|largest| &name < largest) {
            names.pop();
            names.push(name);
        }
    }

    let mut names = names.into_vec();
    names.sort_unstable();
    if names.is_empty() {
        return Ok(String::new());
    }
    let start = offset - 1;
    if start >= names.len() {
        anyhow::bail!("offset exceeds directory entry count");
    }
    let end = desired_end.min(names.len());
    let mut page = names[start..end].to_vec();
    if total_entries > desired_end {
        page.push(format!(
            "[truncated: directory has more than {desired_end} entries; use offset={}]",
            desired_end + 1
        ));
    }
    Ok(page.join("\n"))
}

async fn list_dir_deep(
    root: &Path,
    path: &Path,
    offset: usize,
    limit: usize,
    desired_end: usize,
    depth: usize,
) -> Result<String> {
    let mut entries = Vec::new();
    let truncated = collect_deep_dir_entries(root, path, depth, &mut entries).await?;
    entries.sort_unstable();
    if entries.is_empty() {
        return Ok(String::new());
    }
    let start = offset - 1;
    if start >= entries.len() {
        anyhow::bail!("offset exceeds directory entry count");
    }

    let end = start.saturating_add(limit).min(entries.len());
    let mut page = entries[start..end].to_vec();
    if end < entries.len() || truncated || entries.len() > desired_end {
        page.push(format!(
            "[truncated: directory has more entries; use offset={}]",
            end + 1
        ));
    }
    Ok(page.join("\n"))
}

async fn collect_deep_dir_entries(
    root: &Path,
    path: &Path,
    depth: usize,
    entries: &mut Vec<String>,
) -> Result<bool> {
    let mut queue = VecDeque::new();
    queue.push_back((path.to_path_buf(), PathBuf::new(), depth));
    let mut truncated = false;

    'scan: while let Some((current_dir, prefix, remaining_depth)) = queue.pop_front() {
        let mut read_dir = tokio::fs::read_dir(&current_dir)
            .await
            .with_context(|| format!("failed to list {}", display_path(root, &current_dir)))?;
        let mut current_entries = Vec::new();

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .with_context(|| format!("failed to read {}", display_path(root, &current_dir)))?
        {
            let file_type = entry.file_type().await.with_context(|| {
                format!("failed to inspect {}", entry.file_name().to_string_lossy())
            })?;
            let file_name = entry.file_name();
            let relative_path = if prefix.as_os_str().is_empty() {
                PathBuf::from(&file_name)
            } else {
                prefix.join(&file_name)
            };
            let mut display = relative_path.to_string_lossy().replace('\\', "/");
            if file_type.is_dir() {
                display.push('/');
            }
            current_entries.push((display, entry.path(), relative_path, file_type.is_dir()));
        }

        current_entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (display, entry_path, relative_path, is_dir) in current_entries {
            if entries.len() >= MAX_LIST_DIR_PAGE_WINDOW {
                truncated = true;
                break 'scan;
            }
            if is_dir && remaining_depth > 1 {
                queue.push_back((entry_path, relative_path, remaining_depth - 1));
            }
            entries.push(display);
        }
    }

    Ok(truncated)
}

async fn exec_command(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_exec_command_arguments(arguments)?;
    let command = trimmed_required("command", &args.command)?;
    anyhow::ensure!(
        command.len() <= MAX_COMMAND_BYTES,
        "command exceeds {MAX_COMMAND_BYTES} bytes"
    );

    let cwd = command_cwd(root, args.cwd.as_deref()).await?;
    let timeout_ms = bounded_u64(
        args.timeout_ms,
        DEFAULT_COMMAND_TIMEOUT_MS,
        MAX_COMMAND_TIMEOUT_MS,
    );
    let max_output_bytes = bounded_limit(
        args.max_output_bytes,
        DEFAULT_COMMAND_OUTPUT_BYTES,
        MAX_COMMAND_OUTPUT_BYTES,
    );
    let result = run_process(
        root,
        "bash",
        &["-lc".to_string(), command.to_string()],
        &cwd,
        timeout_ms,
        max_output_bytes,
        cancellation,
    )
    .await?;
    Ok(process_result_output(result))
}

async fn git_status(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let _args = parse_git_status_arguments(arguments)?;
    let args = vec![
        "status".to_string(),
        "--short".to_string(),
        "--branch".to_string(),
    ];
    let result = run_git(root, args, DEFAULT_COMMAND_OUTPUT_BYTES, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_diff(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_diff_arguments(arguments)?;
    let max_output_bytes = bounded_limit(
        args.max_output_bytes,
        DEFAULT_COMMAND_OUTPUT_BYTES,
        MAX_COMMAND_OUTPUT_BYTES,
    );
    let mut git_args = vec!["diff".to_string()];
    if args.staged {
        git_args.push("--cached".to_string());
    }
    if let Some(path) = args.path.as_deref().filter(|path| !path.trim().is_empty()) {
        git_args.push("--".to_string());
        git_args.push(git_pathspec(path)?);
    }

    let result = run_git(root, git_args, max_output_bytes, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_log(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_log_arguments(arguments)?;
    let max_count = bounded_limit(args.max_count, DEFAULT_GIT_LOG_COUNT, MAX_GIT_LOG_COUNT);
    let git_args = vec![
        "log".to_string(),
        "--oneline".to_string(),
        "--decorate".to_string(),
        "-n".to_string(),
        max_count.to_string(),
    ];

    let result = run_git(root, git_args, DEFAULT_COMMAND_OUTPUT_BYTES, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_query(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_query_arguments(arguments)?;
    let max_output_bytes = bounded_limit(
        args.max_output_bytes,
        DEFAULT_COMMAND_OUTPUT_BYTES,
        MAX_COMMAND_OUTPUT_BYTES,
    );
    let git_args = read_only_git_args(&args.command, &args.args)?;
    let result = run_git(root, git_args, max_output_bytes, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_add(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_add_arguments(arguments)?;
    let pathspecs = git_pathspecs(&args.paths)?;
    let mut git_args = vec!["add".to_string(), "--".to_string()];
    git_args.extend(pathspecs);
    let result = run_git(root, git_args, DEFAULT_COMMAND_OUTPUT_BYTES, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_restore(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_restore_arguments(arguments)?;
    let pathspecs = git_pathspecs(&args.paths)?;
    let mut git_args = vec!["restore".to_string()];
    if args.staged {
        git_args.push("--staged".to_string());
    } else {
        git_args.push("--worktree".to_string());
    }
    git_args.push("--".to_string());
    git_args.extend(pathspecs);
    let result = run_git(root, git_args, DEFAULT_COMMAND_OUTPUT_BYTES, cancellation).await?;
    Ok(process_result_output(result))
}

async fn git_commit(
    root: &Path,
    arguments: &str,
    cancellation: &TurnCancellation,
) -> Result<ToolOutput> {
    let args = parse_git_commit_arguments(arguments)?;
    let message = trimmed_required("message", &args.message)?;
    anyhow::ensure!(
        message.len() <= MAX_GIT_COMMIT_MESSAGE_BYTES,
        "message exceeds {MAX_GIT_COMMIT_MESSAGE_BYTES} bytes"
    );
    let pathspecs = git_pathspecs(&args.paths)?;

    let mut add_args = vec!["add".to_string(), "--".to_string()];
    add_args.extend(pathspecs.iter().cloned());
    let add_result = run_git(root, add_args, DEFAULT_COMMAND_OUTPUT_BYTES, cancellation).await?;
    let add_output = process_result_output(add_result);
    if !add_output.success {
        return Ok(ToolOutput {
            output: format!("git add:\n{}", add_output.output),
            success: false,
        });
    }

    let mut commit_args = vec![
        "commit".to_string(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    commit_args.extend(pathspecs);
    let commit_result = run_git(
        root,
        commit_args,
        DEFAULT_COMMAND_OUTPUT_BYTES,
        cancellation,
    )
    .await?;
    let commit_output = process_result_output(commit_result);

    Ok(ToolOutput {
        success: commit_output.success,
        output: format!(
            "git add:\n{}\n\ngit commit:\n{}",
            add_output.output, commit_output.output
        ),
    })
}

async fn apply_patch(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_apply_patch_arguments(arguments)?;
    anyhow::ensure!(
        args.patch.len() <= MAX_PATCH_BYTES,
        "patch exceeds {MAX_PATCH_BYTES} bytes"
    );

    let patch = ParsedPatch::parse(&args.patch)?;
    let changes = prepare_patch_changes(root, patch).await?;
    apply_prepared_changes(root, &changes).await?;

    Ok(format_patch_summary(&changes))
}

async fn apply_prepared_changes(root: &Path, changes: &[PreparedChange]) -> Result<()> {
    for (applied, change) in changes.iter().enumerate() {
        if let Err(error) = apply_prepared_change(root, change).await {
            if let Err(rollback_error) = rollback_applied_changes(root, &changes[..applied]).await {
                cleanup_prepared_changes(changes).await;
                return Err(error).with_context(|| {
                    format!("failed to roll back patch after apply failure: {rollback_error}")
                });
            }
            cleanup_prepared_changes(changes).await;
            return Err(error);
        }
    }

    cleanup_prepared_changes(changes).await;
    Ok(())
}

async fn apply_prepared_change(root: &Path, change: &PreparedChange) -> Result<()> {
    match change {
        PreparedChange::Write {
            path, temp_path, ..
        } => commit_temp_file(path, temp_path).await,
        PreparedChange::Delete { path, .. } => tokio::fs::remove_file(path)
            .await
            .with_context(|| format!("failed to delete {}", display_path(root, path))),
    }
}

async fn prepare_patch_changes(root: &Path, patch: ParsedPatch) -> Result<Vec<PreparedChange>> {
    anyhow::ensure!(
        !patch.operations.is_empty(),
        "patch contains no file operations"
    );
    let mut changes = Vec::with_capacity(patch.operations.len());
    let mut seen = HashSet::with_capacity(patch.operations.len());

    for operation in patch.operations {
        let change = match prepare_patch_change(root, operation, &mut seen).await {
            Ok(change) => change,
            Err(error) => {
                cleanup_prepared_changes(&changes).await;
                return Err(error);
            }
        };
        changes.push(change);
    }

    Ok(changes)
}

async fn prepare_patch_change(
    root: &Path,
    operation: PatchOperation,
    seen: &mut HashSet<PathBuf>,
) -> Result<PreparedChange> {
    match operation {
        PatchOperation::Add { path, content } => {
            let path = resolve_new_workspace_path(root, &path)?;
            ensure_unique_patch_path(seen, &path)?;
            ensure_new_file_target(root, &path).await?;
            ensure_patch_file_size(content.len() as u64)?;
            let after_bytes = content.len();
            let temp_path = write_temp_sibling(&path, &content, None).await?;
            Ok(PreparedChange::Write {
                display_path: display_path(root, &path),
                before_bytes: None,
                after_bytes,
                path,
                temp_path,
                backup_path: None,
            })
        }
        PatchOperation::Update { path, hunks } => {
            let path = resolve_workspace_path(root, &path)?;
            ensure_unique_patch_path(seen, &path)?;
            let (content, permissions) = read_patch_target(root, &path).await?;
            let before_bytes = content.len();
            let backup_path =
                write_temp_sibling(&path, &content, Some(permissions.clone())).await?;
            let content = apply_hunks(&content, &hunks, &display_path(root, &path))?;
            ensure_patch_file_size(content.len() as u64)?;
            let after_bytes = content.len();
            let temp_path = write_temp_sibling(&path, &content, Some(permissions)).await?;
            Ok(PreparedChange::Write {
                display_path: display_path(root, &path),
                before_bytes: Some(before_bytes),
                after_bytes,
                path,
                temp_path,
                backup_path: Some(backup_path),
            })
        }
        PatchOperation::Delete { path } => {
            let path = resolve_workspace_path(root, &path)?;
            ensure_unique_patch_path(seen, &path)?;
            let (content, permissions) = read_patch_target(root, &path).await?;
            let backup_path = write_temp_sibling(&path, &content, Some(permissions)).await?;
            Ok(PreparedChange::Delete {
                display_path: display_path(root, &path),
                before_bytes: content.len(),
                path,
                backup_path,
            })
        }
    }
}

fn parse_read_file_arguments(arguments: &str) -> Result<ReadFileArguments> {
    parse_tool_arguments(READ_FILE_TOOL, arguments)
}

fn parse_list_dir_arguments(arguments: &str) -> Result<ListDirArguments> {
    parse_tool_arguments(LIST_DIR_TOOL, arguments)
}

fn parse_read_file_range_arguments(arguments: &str) -> Result<ReadFileRangeArguments> {
    parse_tool_arguments(READ_FILE_RANGE_TOOL, arguments)
}

fn parse_search_files_arguments(arguments: &str) -> Result<SearchFilesArguments> {
    parse_tool_arguments(SEARCH_FILES_TOOL, arguments)
}

fn parse_search_text_arguments(arguments: &str) -> Result<SearchTextArguments> {
    parse_tool_arguments(SEARCH_TEXT_TOOL, arguments)
}

fn parse_apply_patch_arguments(arguments: &str) -> Result<ApplyPatchArguments> {
    parse_tool_arguments(APPLY_PATCH_TOOL, arguments)
}

fn parse_exec_command_arguments(arguments: &str) -> Result<ExecCommandArguments> {
    parse_tool_arguments(EXEC_COMMAND_TOOL, arguments)
}

fn parse_git_status_arguments(arguments: &str) -> Result<GitStatusArguments> {
    parse_tool_arguments(GIT_STATUS_TOOL, arguments)
}

fn parse_git_diff_arguments(arguments: &str) -> Result<GitDiffArguments> {
    parse_tool_arguments(GIT_DIFF_TOOL, arguments)
}

fn parse_git_log_arguments(arguments: &str) -> Result<GitLogArguments> {
    parse_tool_arguments(GIT_LOG_TOOL, arguments)
}

fn parse_git_query_arguments(arguments: &str) -> Result<GitQueryArguments> {
    parse_tool_arguments(GIT_QUERY_TOOL, arguments)
}

fn parse_git_add_arguments(arguments: &str) -> Result<GitAddArguments> {
    parse_tool_arguments(GIT_ADD_TOOL, arguments)
}

fn parse_git_restore_arguments(arguments: &str) -> Result<GitRestoreArguments> {
    parse_tool_arguments(GIT_RESTORE_TOOL, arguments)
}

fn parse_git_commit_arguments(arguments: &str) -> Result<GitCommitArguments> {
    parse_tool_arguments(GIT_COMMIT_TOOL, arguments)
}

fn parse_tool_arguments<T>(tool_name: &str, arguments: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(arguments).with_context(|| format!("invalid {tool_name} arguments"))
}

fn trimmed_required<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{name} cannot be empty");
    Ok(value)
}

fn bounded_limit(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.unwrap_or(default).clamp(1, max)
}

fn bounded_u64(value: Option<u64>, default: u64, max: u64) -> u64 {
    value.unwrap_or(default).clamp(1, max)
}

fn list_dir_page_end(offset: usize, limit: usize) -> Result<usize> {
    let start = offset
        .checked_sub(1)
        .context("offset must be a 1-indexed entry number")?;
    let end = start
        .checked_add(limit)
        .context("list_dir pagination window overflowed")?;
    anyhow::ensure!(
        end <= MAX_LIST_DIR_PAGE_WINDOW,
        "list_dir pagination window exceeds {MAX_LIST_DIR_PAGE_WINDOW} entries"
    );
    Ok(end)
}

fn parse_grep_mode(mode: Option<&str>) -> Result<GrepMode> {
    let mode = mode.map(str::trim).filter(|mode| !mode.is_empty());
    let mode = mode.map(str::to_ascii_lowercase);
    match mode.as_deref() {
        None | Some("plain") | Some("literal") => Ok(GrepMode::PlainText),
        Some("regex") => Ok(GrepMode::Regex),
        Some("fuzzy") => Ok(GrepMode::Fuzzy),
        Some(mode) => anyhow::bail!("unsupported search_text mode {mode:?}"),
    }
}

struct BoundedReadLine {
    bytes: Vec<u8>,
    truncated: bool,
}

impl BoundedReadLine {
    fn into_string(self) -> String {
        let mut line = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            line.push_str(READ_LINE_TRUNCATION_SUFFIX);
        }
        line
    }
}

async fn read_bounded_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<BoundedReadLine>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes);
    let mut truncated = false;
    let mut read_any = false;

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !read_any {
                return Ok(None);
            }
            if bytes.last().is_some_and(|byte| *byte == b'\r') {
                bytes.pop();
            }
            return Ok(Some(BoundedReadLine { bytes, truncated }));
        }

        read_any = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consume = newline.map_or(available.len(), |index| index + 1);
        let content = newline.map_or(consume, |_| consume - 1);
        if bytes.len() < max_bytes {
            let remaining = max_bytes - bytes.len();
            let copy = content.min(remaining);
            bytes.extend_from_slice(&available[..copy]);
            truncated |= content > remaining;
        } else {
            truncated |= content > 0;
        }
        reader.consume(consume);

        if newline.is_some() {
            if bytes.last().is_some_and(|byte| *byte == b'\r') {
                bytes.pop();
            }
            return Ok(Some(BoundedReadLine { bytes, truncated }));
        }
    }
}

fn format_line_read_result(
    root: &Path,
    path: &Path,
    offset: usize,
    line_count: usize,
    lines: &[String],
    has_more: bool,
    cut_by_bytes: bool,
) -> String {
    let mut output = String::new();
    output.push_str("<path>");
    output.push_str(&display_path(root, path));
    output.push_str("</path>\n<type>file</type>\n<content>\n");

    for (index, line) in lines.iter().enumerate() {
        output.push_str(&(offset + index).to_string());
        output.push_str(": ");
        output.push_str(line);
        output.push('\n');
    }

    let last_line = offset.saturating_add(lines.len()).saturating_sub(1);
    if cut_by_bytes {
        let next_offset = last_line.saturating_add(1);
        output.push_str(&format!(
            "\n(Output capped at {MAX_FILE_BYTES} bytes. Showing lines {offset}-{last_line}. Use offset={next_offset} to continue.)"
        ));
    } else if has_more {
        let next_offset = last_line.saturating_add(1);
        output.push_str(&format!(
            "\n(Showing lines {offset}-{last_line}. Use offset={next_offset} to continue.)"
        ));
    } else {
        output.push_str(&format!("\n(End of file - total {line_count} lines)"));
    }

    output.push_str("\n</content>");
    output
}

struct CappedText {
    output: String,
    max_bytes: usize,
    truncated: bool,
}

impl CappedText {
    fn new(max_bytes: usize) -> Self {
        Self {
            output: String::with_capacity(max_bytes.min(8192)),
            max_bytes,
            truncated: false,
        }
    }

    fn push_str(&mut self, value: &str) -> bool {
        if self.output.len() >= self.max_bytes {
            self.truncated = true;
            return false;
        }
        let remaining = self.max_bytes - self.output.len();
        if value.len() <= remaining {
            self.output.push_str(value);
            return true;
        }

        let mut end = remaining;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        self.output.push_str(&value[..end]);
        self.truncated = true;
        false
    }

    fn push_line(&mut self, value: &str) -> bool {
        self.push_str(value) && self.push_str("\n")
    }

    fn finish(mut self, label: &str) -> String {
        if self.truncated {
            self.output.push_str(&format!(
                "\n[truncated: {label} exceeds {} bytes]",
                self.max_bytes
            ));
        }
        self.output
    }
}

async fn command_cwd(root: &Path, cwd: Option<&str>) -> Result<PathBuf> {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return Ok(root.to_path_buf());
    };
    let path = resolve_workspace_path(root, cwd)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, &path)))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "{} is not a directory",
        display_path(root, &path)
    );
    Ok(path)
}

fn git_pathspec(path: &str) -> Result<String> {
    let relative = clean_workspace_relative_path(path)?;
    Ok(format!(":(literal){}", relative.to_string_lossy()))
}

fn git_pathspecs(paths: &[String]) -> Result<Vec<String>> {
    anyhow::ensure!(!paths.is_empty(), "paths cannot be empty");
    anyhow::ensure!(
        paths.len() <= MAX_GIT_PATHS,
        "paths exceed {MAX_GIT_PATHS} entries"
    );
    let mut pathspecs = Vec::with_capacity(paths.len());
    let mut path_bytes = 0usize;
    for path in paths {
        let pathspec = git_pathspec(path)?;
        path_bytes = path_bytes.saturating_add(pathspec.len());
        anyhow::ensure!(
            path_bytes <= MAX_GIT_PATH_BYTES,
            "paths exceed {MAX_GIT_PATH_BYTES} bytes"
        );
        pathspecs.push(pathspec);
    }
    Ok(pathspecs)
}

fn read_only_git_args(command: &str, args: &[String]) -> Result<Vec<String>> {
    let command = trimmed_required("command", command)?.to_ascii_lowercase();
    validate_git_query_args(args)?;
    let git_args = match command.as_str() {
        "status" | "log" | "blame" | "ls-files" | "rev-parse" | "merge-base" | "describe" => {
            let mut git_args = Vec::with_capacity(args.len() + 1);
            git_args.push(command);
            git_args.extend(args.iter().cloned());
            git_args
        }
        "diff" | "show" => {
            let mut git_args = Vec::with_capacity(args.len() + 2);
            git_args.push(command);
            git_args.push("--no-ext-diff".to_string());
            git_args.extend(args.iter().cloned());
            git_args
        }
        "grep" => {
            ensure_git_grep_args_are_read_only(args)?;
            let mut git_args = Vec::with_capacity(args.len() + 1);
            git_args.push(command);
            git_args.extend(args.iter().cloned());
            git_args
        }
        "branch" => {
            anyhow::ensure!(
                args.is_empty()
                    || (args.len() == 1
                        && args.first().is_some_and(|arg| arg == "--show-current")),
                "git_query branch only supports --show-current"
            );
            vec!["branch".to_string(), "--show-current".to_string()]
        }
        "worktree" => {
            anyhow::ensure!(
                matches!(args.first().map(String::as_str), Some("list")),
                "git_query worktree only supports list"
            );
            ensure_git_worktree_list_args(args)?;
            let mut git_args = Vec::with_capacity(args.len() + 1);
            git_args.push(command);
            git_args.extend(args.iter().cloned());
            git_args
        }
        "submodule" => {
            if args.is_empty() {
                vec!["submodule".to_string(), "status".to_string()]
            } else {
                anyhow::ensure!(
                    matches!(args.first().map(String::as_str), Some("status")),
                    "git_query submodule only supports status"
                );
                let mut git_args = Vec::with_capacity(args.len() + 1);
                git_args.push(command);
                git_args.extend(args.iter().cloned());
                git_args
            }
        }
        _ => anyhow::bail!(
            "unsupported git_query command {command:?}; allowed commands: status, diff, log, show, blame, grep, ls-files, branch, rev-parse, merge-base, describe, worktree, submodule"
        ),
    };
    deny_git_query_output_file_args(&git_args)?;
    Ok(git_args)
}

fn validate_git_query_args(args: &[String]) -> Result<()> {
    anyhow::ensure!(
        args.len() <= MAX_GIT_QUERY_ARGS,
        "git_query args exceed {MAX_GIT_QUERY_ARGS} entries"
    );
    let mut bytes = 0usize;
    for arg in args {
        anyhow::ensure!(
            !arg.is_empty(),
            "git_query args cannot contain empty strings"
        );
        anyhow::ensure!(
            !arg.contains('\0'),
            "git_query args cannot contain NUL bytes"
        );
        bytes = bytes.saturating_add(arg.len());
        anyhow::ensure!(
            bytes <= MAX_GIT_QUERY_ARG_BYTES,
            "git_query args exceed {MAX_GIT_QUERY_ARG_BYTES} bytes"
        );
    }
    Ok(())
}

fn ensure_git_grep_args_are_read_only(args: &[String]) -> Result<()> {
    for arg in args {
        anyhow::ensure!(
            arg != "-O" && !arg.starts_with("-O") && !arg.starts_with("--open-files-in-pager"),
            "git_query grep does not support pager-opening options"
        );
    }
    Ok(())
}

fn ensure_git_worktree_list_args(args: &[String]) -> Result<()> {
    for arg in args.iter().skip(1) {
        anyhow::ensure!(
            matches!(arg.as_str(), "--porcelain" | "-z" | "-v" | "--verbose"),
            "git_query worktree list only supports --porcelain, -z, -v, and --verbose"
        );
    }
    Ok(())
}

fn deny_git_query_output_file_args(args: &[String]) -> Result<()> {
    for arg in args {
        anyhow::ensure!(
            arg != "--output" && !arg.starts_with("--output="),
            "git_query does not support options that write output files"
        );
    }
    Ok(())
}

async fn run_git(
    root: &Path,
    args: Vec<String>,
    max_output_bytes: usize,
    cancellation: &TurnCancellation,
) -> Result<ProcessResult> {
    run_process(
        root,
        "git",
        &args,
        root,
        DEFAULT_COMMAND_TIMEOUT_MS,
        max_output_bytes,
        cancellation,
    )
    .await
}

async fn run_process(
    root: &Path,
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout_ms: u64,
    max_output_bytes: usize,
    cancellation: &TurnCancellation,
) -> Result<ProcessResult> {
    let display = display_command(program, args);
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {display:?}"))?;
    let child_id = child.id();
    let stdout = child
        .stdout
        .take()
        .context("failed to capture command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture command stderr")?;
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let total_output_bytes = Arc::new(AtomicUsize::new(0));
    let output_dir = root.join(COMMAND_OUTPUT_DIR);
    let stdout_task = tokio::spawn(read_limited_output(
        root.to_path_buf(),
        output_dir.clone(),
        "stdout".to_string(),
        stdout,
        max_output_bytes,
        Arc::clone(&total_output_bytes),
        Arc::clone(&output_limit_exceeded),
    ));
    let stderr_task = tokio::spawn(read_limited_output(
        root.to_path_buf(),
        output_dir,
        "stderr".to_string(),
        stderr,
        max_output_bytes,
        Arc::clone(&total_output_bytes),
        Arc::clone(&output_limit_exceeded),
    ));

    let started = Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let mut killed_for_output = false;
    let status = loop {
        if cancellation.is_cancelled() {
            cancelled = true;
            kill_process(child_id, &mut child).await;
            break Some(child.wait().await.context("failed to wait for command")?);
        }
        if output_limit_exceeded.load(Ordering::Relaxed) {
            killed_for_output = true;
            kill_process(child_id, &mut child).await;
            break Some(child.wait().await.context("failed to wait for command")?);
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            timed_out = true;
            kill_process(child_id, &mut child).await;
            break Some(child.wait().await.context("failed to wait for command")?);
        }

        if let Ok(status) = tokio::time::timeout(COMMAND_WAIT_POLL_INTERVAL, child.wait()).await {
            break Some(status.context("failed to wait for command")?);
        }
    };

    let stdout = stdout_task
        .await
        .context("stdout reader task failed")?
        .context("failed to read command stdout")?;
    let stderr = stderr_task
        .await
        .context("stderr reader task failed")?
        .context("failed to read command stderr")?;
    let output_limit_exceeded = killed_for_output || output_limit_exceeded.load(Ordering::Relaxed);

    Ok(ProcessResult {
        display,
        cwd: display_path(root, cwd),
        timeout_ms,
        status,
        timed_out,
        cancelled,
        output_limit_exceeded,
        stdout,
        stderr,
    })
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

async fn kill_process(child_id: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(child_id) = child_id {
        unsafe {
            let _ = libc::kill(-(child_id as libc::pid_t), libc::SIGKILL);
        }
        return;
    }

    let _ = child.kill().await;
}

async fn read_limited_output<R>(
    root: PathBuf,
    artifact_dir: PathBuf,
    stream_name: String,
    mut reader: R,
    max_bytes: usize,
    total_output_bytes: Arc<AtomicUsize>,
    output_limit_exceeded: Arc<AtomicBool>,
) -> Result<LimitedOutput>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut output = LimitedOutput {
        bytes: Vec::with_capacity(max_bytes.min(8192)),
        total_bytes: 0,
        artifact_path: None,
    };
    let mut buffer = [0u8; 8192];
    let mut artifact: Option<CommandOutputArtifact> = None;

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let chunk = &buffer[..bytes_read];
        output.total_bytes = output.total_bytes.saturating_add(bytes_read);
        let total_bytes = total_output_bytes.fetch_add(bytes_read, Ordering::Relaxed) + bytes_read;

        if artifact.is_none() && output.total_bytes > max_bytes {
            let mut next_artifact =
                create_command_output_artifact(&root, &artifact_dir, &stream_name).await?;
            next_artifact.file.write_all(&output.bytes).await?;
            artifact = Some(next_artifact);
        }
        if let Some(artifact) = artifact.as_mut() {
            artifact.file.write_all(chunk).await?;
        }

        output.bytes.extend_from_slice(chunk);
        if output.bytes.len() > max_bytes {
            let overflow = output.bytes.len() - max_bytes;
            output.bytes.drain(..overflow);
        }

        if total_bytes > MAX_COMMAND_TOTAL_OUTPUT_BYTES {
            output_limit_exceeded.store(true, Ordering::Relaxed);
            break;
        }
    }

    if let Some(mut artifact) = artifact {
        artifact.file.flush().await?;
        output.artifact_path = Some(artifact.display_path);
    }
    Ok(output)
}

struct CommandOutputArtifact {
    display_path: String,
    file: tokio::fs::File,
}

async fn create_command_output_artifact(
    root: &Path,
    artifact_dir: &Path,
    stream_name: &str,
) -> Result<CommandOutputArtifact> {
    tokio::fs::create_dir_all(artifact_dir)
        .await
        .with_context(|| format!("failed to create {}", display_path(root, artifact_dir)))?;
    for attempt in 0..16 {
        let path = command_output_artifact_path(artifact_dir, stream_name, attempt);
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => {
                return Ok(CommandOutputArtifact {
                    display_path: display_path(root, &path),
                    file,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", display_path(root, &path)));
            }
        }
    }

    anyhow::bail!(
        "failed to allocate command output artifact in {}",
        display_path(root, artifact_dir)
    )
}

fn command_output_artifact_path(artifact_dir: &Path, stream_name: &str, attempt: usize) -> PathBuf {
    let counter = COMMAND_OUTPUT_ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    artifact_dir.join(format!(
        "{}-{}-{}-{}-{attempt}.log",
        stream_name,
        std::process::id(),
        timestamp_nanos(),
        counter
    ))
}

fn timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[derive(Debug)]
struct ProcessResult {
    display: String,
    cwd: String,
    timeout_ms: u64,
    status: Option<std::process::ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    output_limit_exceeded: bool,
    stdout: LimitedOutput,
    stderr: LimitedOutput,
}

#[derive(Debug)]
struct LimitedOutput {
    bytes: Vec<u8>,
    total_bytes: usize,
    artifact_path: Option<String>,
}

impl LimitedOutput {
    fn is_truncated(&self) -> bool {
        self.total_bytes > self.bytes.len()
    }
}

fn process_result_output(result: ProcessResult) -> ToolOutput {
    let success = result
        .status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success)
        && !result.timed_out
        && !result.cancelled
        && !result.output_limit_exceeded;
    let output = format_process_result(&result);
    ToolOutput { output, success }
}

fn format_process_result(result: &ProcessResult) -> String {
    let mut output = String::new();
    output.push_str("command: ");
    output.push_str(&result.display);
    output.push('\n');
    output.push_str("cwd: ");
    output.push_str(if result.cwd.is_empty() {
        "."
    } else {
        &result.cwd
    });
    output.push('\n');
    if result.timed_out {
        output.push_str(&format!(
            "status: timed out after {} ms\n",
            result.timeout_ms
        ));
    } else if result.cancelled {
        output.push_str("status: cancelled\n");
    } else if result.output_limit_exceeded {
        output.push_str(&format!(
            "status: output exceeded {} bytes; process killed\n",
            MAX_COMMAND_TOTAL_OUTPUT_BYTES
        ));
    } else if let Some(status) = result.status.as_ref() {
        output.push_str(&format!("status: {}\n", format_exit_status(status)));
    }
    push_limited_section(&mut output, "stdout", &result.stdout);
    push_limited_section(&mut output, "stderr", &result.stderr);
    output
}

fn push_limited_section(output: &mut String, name: &str, limited: &LimitedOutput) {
    output.push_str(name);
    output.push_str(":\n");
    if limited.is_truncated() {
        if let Some(path) = limited.artifact_path.as_ref() {
            output.push_str(&format!(
                "[truncated: {name} showing last {} of {} bytes; full output saved to {path}]\n",
                limited.bytes.len(),
                limited.total_bytes
            ));
        } else {
            output.push_str(&format!(
                "[truncated: {name} showing last {} of {} bytes]\n",
                limited.bytes.len(),
                limited.total_bytes
            ));
        }
    }
    output.push_str(&String::from_utf8_lossy(&limited.bytes));
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

fn format_exit_status(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => "terminated by signal".to_string(),
    }
}

fn display_command(program: &str, args: &[String]) -> String {
    let mut display = shell_display_token(program);
    for arg in args {
        display.push(' ');
        display.push_str(&shell_display_token(arg));
    }
    display
}

fn shell_display_token(token: &str) -> String {
    if token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        return token.to_string();
    }
    format!("'{}'", token.replace('\'', "'\\''"))
}

fn format_file_search_results(
    picker: &FilePicker,
    results: &fff_search::MixedSearchResult<'_>,
    offset: usize,
    limit: usize,
) -> String {
    if results.items.is_empty() {
        return format!(
            "No files or directories matched. total_files={} total_dirs={}",
            results.total_files, results.total_dirs
        );
    }

    let mut output = String::new();
    for item in &results.items {
        match item {
            MixedItemRef::File(file) => {
                output.push_str(&file.relative_path(picker));
            }
            MixedItemRef::Dir(dir) => {
                output.push_str(&dir.relative_path(picker));
                if !output.ends_with(std::path::MAIN_SEPARATOR) {
                    output.push(std::path::MAIN_SEPARATOR);
                }
            }
        }
        output.push('\n');
    }
    output.push_str(&format!(
        "[matched={} total_files={} total_dirs={} offset={} limit={}]",
        results.total_matched, results.total_files, results.total_dirs, offset, limit
    ));
    output
}

fn format_text_search_results(
    picker: &FilePicker,
    results: &fff_search::grep::GrepResult<'_>,
) -> String {
    if results.matches.is_empty() {
        return format!(
            "No text matched. searched_files={} searchable_files={} total_files={}",
            results.total_files_searched, results.filtered_file_count, results.total_files
        );
    }

    let mut output = CappedText::new(MAX_SEARCH_TEXT_OUTPUT_BYTES);
    if let Some(error) = &results.regex_fallback_error {
        output.push_str("Regex fallback: ");
        output.push_str(error);
        output.push_str("\n");
    }
    for search_match in &results.matches {
        let Some(file) = results.files.get(search_match.file_index) else {
            continue;
        };
        if !output.push_line(&format!(
            "{}:{}:{}: {}",
            file.relative_path(picker),
            search_match.line_number,
            search_match.col.saturating_add(1),
            search_match.line_content
        )) {
            break;
        }
        for line in &search_match.context_before {
            if !output.push_str("  before: ") || !output.push_line(line) {
                break;
            }
        }
        for line in &search_match.context_after {
            if !output.push_str("  after: ") || !output.push_line(line) {
                break;
            }
        }
    }
    output.push_str(&format!(
        "[matches={} files_with_matches={} searched_files={} searchable_files={} next_file_offset={}]",
        results.matches.len(),
        results.files_with_matches,
        results.total_files_searched,
        results.filtered_file_count,
        results.next_file_offset,
    ));
    output.finish("search_text output")
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedPatch {
    operations: Vec<PatchOperation>,
}

impl ParsedPatch {
    fn parse(patch: &str) -> Result<Self> {
        let lines = patch.split_inclusive('\n').collect::<Vec<_>>();
        anyhow::ensure!(!lines.is_empty(), "patch is empty");
        anyhow::ensure!(
            patch_line_body(lines[0]) == "*** Begin Patch",
            "patch must start with *** Begin Patch"
        );

        let mut index = 1;
        let mut operations = Vec::new();
        while index < lines.len() {
            let line = patch_line_body(lines[index]);
            if line == "*** End Patch" {
                anyhow::ensure!(
                    index + 1 == lines.len(),
                    "patch contains data after *** End Patch"
                );
                return Ok(Self { operations });
            }

            if let Some(path) = line.strip_prefix("*** Add File: ") {
                index += 1;
                operations.push(parse_add_file_operation(&lines, &mut index, path)?);
            } else if let Some(path) = line.strip_prefix("*** Update File: ") {
                index += 1;
                operations.push(parse_update_file_operation(&lines, &mut index, path)?);
            } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
                index += 1;
                operations.push(PatchOperation::Delete {
                    path: parse_patch_path(path)?,
                });
            } else {
                anyhow::bail!("unsupported patch header {line:?}");
            }
        }

        anyhow::bail!("patch is missing *** End Patch")
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PatchOperation {
    Add { path: String, content: String },
    Update { path: String, hunks: Vec<PatchHunk> },
    Delete { path: String },
}

#[derive(Debug, PartialEq, Eq)]
struct PatchHunk {
    old: String,
    new: String,
}

#[derive(Debug)]
enum PreparedChange {
    Write {
        display_path: String,
        before_bytes: Option<usize>,
        after_bytes: usize,
        path: PathBuf,
        temp_path: PathBuf,
        backup_path: Option<PathBuf>,
    },
    Delete {
        display_path: String,
        before_bytes: usize,
        path: PathBuf,
        backup_path: PathBuf,
    },
}

fn parse_add_file_operation(
    lines: &[&str],
    index: &mut usize,
    path: &str,
) -> Result<PatchOperation> {
    let mut content = String::new();
    while *index < lines.len() && !is_patch_operation_header(lines[*index]) {
        let line = lines[*index];
        anyhow::ensure!(
            line.starts_with('+'),
            "add file lines must start with + for {path:?}"
        );
        content.push_str(&line[1..]);
        *index += 1;
    }

    Ok(PatchOperation::Add {
        path: parse_patch_path(path)?,
        content,
    })
}

fn parse_update_file_operation(
    lines: &[&str],
    index: &mut usize,
    path: &str,
) -> Result<PatchOperation> {
    let mut hunks = Vec::new();
    while *index < lines.len() && !is_patch_operation_header(lines[*index]) {
        anyhow::ensure!(
            is_hunk_header(lines[*index]),
            "update file sections must contain @@ hunks for {path:?}"
        );
        *index += 1;
        hunks.push(parse_patch_hunk(lines, index, path)?);
    }

    anyhow::ensure!(!hunks.is_empty(), "update file {path:?} contains no hunks");
    Ok(PatchOperation::Update {
        path: parse_patch_path(path)?,
        hunks,
    })
}

fn parse_patch_hunk(lines: &[&str], index: &mut usize, path: &str) -> Result<PatchHunk> {
    let mut old = String::new();
    let mut new = String::new();
    let mut changed = false;

    while *index < lines.len()
        && !is_patch_operation_header(lines[*index])
        && !is_hunk_header(lines[*index])
    {
        let line = lines[*index];
        if let Some(rest) = line.strip_prefix(' ') {
            old.push_str(rest);
            new.push_str(rest);
        } else if let Some(rest) = line.strip_prefix('-') {
            old.push_str(rest);
            changed = true;
        } else if let Some(rest) = line.strip_prefix('+') {
            new.push_str(rest);
            changed = true;
        } else if line.is_empty() {
            anyhow::bail!("empty hunk line in {path:?}");
        } else {
            anyhow::bail!("hunk lines must start with space, -, or + in {path:?}");
        }
        *index += 1;
    }

    anyhow::ensure!(changed, "hunk in {path:?} contains no changes");
    anyhow::ensure!(
        !old.is_empty(),
        "hunk in {path:?} must include context or removed lines"
    );
    Ok(PatchHunk { old, new })
}

fn parse_patch_path(path: &str) -> Result<String> {
    let path = path.trim();
    anyhow::ensure!(!path.is_empty(), "patch path cannot be empty");
    Ok(path.to_string())
}

fn patch_line_body(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn is_patch_operation_header(line: &str) -> bool {
    let line = patch_line_body(line);
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Update File: ")
        || line.starts_with("*** Delete File: ")
}

fn is_hunk_header(line: &str) -> bool {
    patch_line_body(line).starts_with("@@")
}

fn apply_hunks(content: &str, hunks: &[PatchHunk], path: &str) -> Result<String> {
    let mut content = content.to_string();
    for hunk in hunks {
        let index = unique_match_index(&content, &hunk.old, path)?;
        content.replace_range(index..index + hunk.old.len(), &hunk.new);
    }
    Ok(content)
}

fn unique_match_index(content: &str, needle: &str, path: &str) -> Result<usize> {
    let mut matches = content.match_indices(needle);
    let Some((index, _)) = matches.next() else {
        anyhow::bail!("patch hunk did not match {path}");
    };
    anyhow::ensure!(
        matches.next().is_none(),
        "patch hunk matched multiple locations in {path}"
    );
    Ok(index)
}

async fn read_patch_target(root: &Path, path: &Path) -> Result<(String, std::fs::Permissions)> {
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("failed to inspect {}", display_path(root, path)))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a file",
        display_path(root, path)
    );
    ensure_patch_file_size(metadata.len())?;

    let permissions = metadata.permissions();
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read {}", display_path(root, path)))?;
    let content = String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", display_path(root, path)))?;
    Ok((content, permissions))
}

async fn ensure_new_file_target(root: &Path, path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => anyhow::bail!("{} already exists", display_path(root, path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", display_path(root, path)))
        }
    }
}

fn ensure_patch_file_size(bytes: u64) -> Result<()> {
    anyhow::ensure!(
        bytes <= MAX_PATCH_FILE_BYTES,
        "patch target exceeds {MAX_PATCH_FILE_BYTES} bytes"
    );
    Ok(())
}

fn ensure_unique_patch_path(seen: &mut HashSet<PathBuf>, path: &Path) -> Result<()> {
    anyhow::ensure!(
        seen.insert(path.to_path_buf()),
        "patch touches {} more than once",
        path.display()
    );
    Ok(())
}

async fn write_temp_sibling(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<PathBuf> {
    let (temp_path, mut file) = create_temp_sibling(path).await?;
    if let Err(error) = file.write_all(content.as_bytes()).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to write {}", temp_path.display()));
    }
    if let Err(error) = file.flush().await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to flush {}", temp_path.display()));
    }
    drop(file);

    if let Some(permissions) = permissions {
        if let Err(error) = tokio::fs::set_permissions(&temp_path, permissions).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error)
                .with_context(|| format!("failed to set permissions on {}", temp_path.display()));
        }
    }

    Ok(temp_path)
}

async fn commit_temp_file(path: &Path, temp_path: &Path) -> Result<()> {
    if let Err(error) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

async fn rollback_applied_changes(root: &Path, changes: &[PreparedChange]) -> Result<()> {
    let mut errors = Vec::new();
    for change in changes.iter().rev() {
        if let Err(error) = rollback_applied_change(root, change).await {
            errors.push(error.to_string());
        }
    }

    anyhow::ensure!(
        errors.is_empty(),
        "rollback failed for {} change(s): {}",
        errors.len(),
        errors.join("; ")
    );
    Ok(())
}

async fn rollback_applied_change(root: &Path, change: &PreparedChange) -> Result<()> {
    match change {
        PreparedChange::Write {
            path,
            backup_path: Some(backup_path),
            ..
        } => restore_backup_file(root, path, backup_path).await,
        PreparedChange::Write {
            path,
            backup_path: None,
            ..
        } => remove_file_if_exists(path)
            .await
            .with_context(|| format!("failed to remove added {}", display_path(root, path))),
        PreparedChange::Delete {
            path, backup_path, ..
        } => restore_backup_file(root, path, backup_path).await,
    }
}

async fn restore_backup_file(root: &Path, path: &Path, backup_path: &Path) -> Result<()> {
    remove_file_if_exists(path)
        .await
        .with_context(|| format!("failed to clear {}", display_path(root, path)))?;
    tokio::fs::rename(backup_path, path)
        .await
        .with_context(|| format!("failed to restore {}", display_path(root, path)))
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn cleanup_prepared_changes(changes: &[PreparedChange]) {
    for change in changes {
        match change {
            PreparedChange::Write {
                temp_path,
                backup_path,
                ..
            } => {
                let _ = tokio::fs::remove_file(temp_path).await;
                if let Some(backup_path) = backup_path {
                    let _ = tokio::fs::remove_file(backup_path).await;
                }
            }
            PreparedChange::Delete { backup_path, .. } => {
                let _ = tokio::fs::remove_file(backup_path).await;
            }
        }
    }
}

async fn create_temp_sibling(path: &Path) -> Result<(PathBuf, tokio::fs::File)> {
    for attempt in 0..16 {
        let temp_path = temp_sibling_path(path, attempt)?;
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", temp_path.display()));
            }
        }
    }
    anyhow::bail!("failed to allocate temporary file for {}", path.display())
}

fn temp_sibling_path(path: &Path, attempt: u8) -> Result<PathBuf> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(
        ".{name}.rust-agent-{}-{unique}-{attempt}.tmp",
        std::process::id()
    )))
}

fn format_patch_summary(changes: &[PreparedChange]) -> String {
    let mut output = format!("Applied patch: {} file(s) changed", changes.len());
    for change in changes {
        output.push('\n');
        match change {
            PreparedChange::Write {
                display_path,
                before_bytes: Some(before_bytes),
                after_bytes,
                ..
            } => {
                output.push_str(&format!(
                    "updated {display_path} ({before_bytes} -> {after_bytes} bytes)"
                ));
            }
            PreparedChange::Write {
                display_path,
                before_bytes: None,
                after_bytes,
                ..
            } => {
                output.push_str(&format!("added {display_path} ({after_bytes} bytes)"));
            }
            PreparedChange::Delete {
                display_path,
                before_bytes,
                ..
            } => {
                output.push_str(&format!("deleted {display_path} ({before_bytes} bytes)"));
            }
        }
    }
    output
}

fn resolve_new_workspace_path(root: &Path, path: &str) -> Result<PathBuf> {
    let relative = clean_workspace_relative_path(path)?;
    let full_path = root.join(&relative);
    let parent = full_path
        .parent()
        .with_context(|| format!("path has no parent: {path}"))?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("failed to resolve parent for path {path}"))?;
    anyhow::ensure!(parent.starts_with(root), "path escapes workspace: {path}");
    let file_name = relative
        .file_name()
        .with_context(|| format!("path has no file name: {path}"))?;
    let full_path = parent.join(file_name);
    anyhow::ensure!(
        full_path.starts_with(root),
        "path escapes workspace: {path}"
    );
    Ok(full_path)
}

fn clean_workspace_relative_path(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim();
    anyhow::ensure!(!trimmed.is_empty(), "path cannot be empty");
    let requested = Path::new(trimmed);
    anyhow::ensure!(
        !requested.is_absolute(),
        "absolute paths are not allowed: {trimmed}"
    );

    let mut clean = PathBuf::new();
    for component in requested.components() {
        match component {
            std::path::Component::Normal(part) => clean.push(part),
            std::path::Component::CurDir => {}
            _ => anyhow::bail!("path escapes workspace: {trimmed}"),
        }
    }
    anyhow::ensure!(!clean.as_os_str().is_empty(), "path cannot be empty");
    Ok(clean)
}

fn resolve_workspace_path(root: &Path, path: &str) -> Result<PathBuf> {
    let trimmed = path.trim();
    anyhow::ensure!(!trimmed.is_empty(), "path cannot be empty");
    let requested = Path::new(trimmed);
    anyhow::ensure!(
        !requested.is_absolute(),
        "absolute paths are not allowed: {trimmed}"
    );

    let full_path = root.join(requested);
    let canonical = full_path
        .canonicalize()
        .with_context(|| format!("failed to resolve path {trimmed}"))?;
    anyhow::ensure!(
        canonical.starts_with(root),
        "path escapes workspace: {trimmed}"
    );
    Ok(canonical)
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::bench_support::DurationSummary;

    #[test]
    fn serializes_tool_specs_without_allocating_json_values() {
        let specs = tool_specs_for_policy(ToolPolicy::ReadOnly);

        assert_eq!(
            serde_json::to_value(&specs).unwrap(),
            json!([
                {
                    "type": "function",
                    "name": "read_file",
                    "description": "Read a UTF-8 text file from the current workspace, optionally by line range.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Workspace-relative path to the file to read.",
                            },
                            "offset": {
                                "type": "integer",
                                "description": "1-indexed line number to start reading from.",
                                "minimum": 1,
                                "maximum": usize::MAX,
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum number of lines to read.",
                                "minimum": 1,
                                "maximum": MAX_READ_LINE_LIMIT,
                            },
                        },
                        "required": ["path"],
                        "additionalProperties": false,
                    },
                },
                {
                    "type": "function",
                    "name": "read_file_range",
                    "description": "Read a byte range from a UTF-8 text file in the current workspace.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Workspace-relative path to the file to read.",
                            },
                            "offset": {
                                "type": "integer",
                                "description": "Byte offset where reading starts. Defaults to 0.",
                                "minimum": 0,
                                "maximum": usize::MAX,
                            },
                            "max_bytes": {
                                "type": "integer",
                                "description": "Maximum bytes to return from the requested offset.",
                                "minimum": 1,
                                "maximum": MAX_FILE_BYTES,
                            },
                        },
                        "required": ["path"],
                        "additionalProperties": false,
                    },
                },
                {
                    "type": "function",
                    "name": "list_dir",
                    "description": "List workspace directory entries with pagination and optional depth.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Workspace-relative directory path. Use . for the workspace root.",
                            },
                            "offset": {
                                "type": "integer",
                                "description": "1-indexed entry number to start listing from.",
                                "minimum": 1,
                                "maximum": MAX_LIST_DIR_PAGE_WINDOW,
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum number of entries to return.",
                                "minimum": 1,
                                "maximum": MAX_LIST_DIR_LIMIT,
                            },
                            "depth": {
                                "type": "integer",
                                "description": "Maximum directory depth to traverse.",
                                "minimum": 1,
                                "maximum": MAX_LIST_DIR_DEPTH,
                            },
                        },
                        "required": ["path"],
                        "additionalProperties": false,
                    },
                },
                {
                    "type": "function",
                    "name": "search_files",
                    "description": "Fuzzy-search indexed workspace files and directories by path.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Fuzzy path query, such as main rs, src tools, or config.",
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum number of results to return.",
                                "minimum": 1,
                                "maximum": MAX_SEARCH_RESULTS,
                            },
                            "offset": {
                                "type": "integer",
                                "description": "Result offset for pagination.",
                                "minimum": 0,
                                "maximum": MAX_SEARCH_OFFSET,
                            },
                        },
                        "required": ["query"],
                        "additionalProperties": false,
                    },
                },
                {
                    "type": "function",
                    "name": "search_text",
                    "description": "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Text query. Path constraints like src/*.rs may be included.",
                            },
                            "mode": {
                                "type": "string",
                                "description": "Search mode: plain, regex, or fuzzy. Defaults to plain.",
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum number of matching lines to return.",
                                "minimum": 1,
                                "maximum": MAX_SEARCH_RESULTS,
                            },
                            "file_offset": {
                                "type": "integer",
                                "description": "File pagination offset returned by an earlier search_text call.",
                                "minimum": 0,
                                "maximum": MAX_SEARCH_OFFSET,
                            },
                            "before_context": {
                                "type": "integer",
                                "description": "Context lines to include before each match.",
                                "minimum": 0,
                                "maximum": MAX_TEXT_CONTEXT_LINES,
                            },
                            "after_context": {
                                "type": "integer",
                                "description": "Context lines to include after each match.",
                                "minimum": 0,
                                "maximum": MAX_TEXT_CONTEXT_LINES,
                            },
                        },
                        "required": ["query"],
                        "additionalProperties": false,
                    },
                },
            ])
        );
    }

    #[test]
    fn search_text_cache_key_tracks_context_arguments() {
        let cache_key = SEARCH_TEXT_TOOL_SPEC.parameters_cache_key();

        assert!(cache_key.contains("before_context:integer"));
        assert!(cache_key.contains("after_context:integer"));
        assert!(!cache_key.contains(":context:integer"));
    }

    #[test]
    fn workspace_write_policy_exposes_apply_patch() {
        let specs = tool_specs_for_policy(ToolPolicy::WorkspaceWrite);

        assert!(specs.iter().any(|spec| spec.name() == APPLY_PATCH_TOOL));
        assert_eq!(
            serde_json::to_value(specs.last().unwrap()).unwrap(),
            json!({
                "type": "function",
                "name": "apply_patch",
                "description": "Apply a workspace-confined patch that adds, updates, or deletes UTF-8 files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": "Patch text using *** Begin Patch / *** End Patch blocks.",
                        },
                    },
                    "required": ["patch"],
                    "additionalProperties": false,
                },
            })
        );
    }

    #[test]
    fn workspace_exec_policy_exposes_command_and_git_tools() {
        let specs = tool_specs_for_policy(ToolPolicy::WorkspaceExec);

        assert!(specs.iter().any(|spec| spec.name() == APPLY_PATCH_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == EXEC_COMMAND_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_STATUS_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_DIFF_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_LOG_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_QUERY_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_ADD_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_RESTORE_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_COMMIT_TOOL));
        let exec_spec = specs
            .iter()
            .find(|spec| spec.name() == EXEC_COMMAND_TOOL)
            .unwrap();
        assert_eq!(
            serde_json::to_value(exec_spec).unwrap(),
            json!({
                "type": "function",
                "name": "exec_command",
                "description": "Execute a bash command from the workspace and return bounded output.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute through bash.",
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Workspace-relative directory to run from. Defaults to .",
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "Maximum command runtime in milliseconds.",
                            "minimum": 1,
                            "maximum": MAX_COMMAND_TIMEOUT_MS as usize,
                        },
                        "max_output_bytes": {
                            "type": "integer",
                            "description": "Maximum stdout bytes and stderr bytes to retain separately.",
                            "minimum": 1,
                            "maximum": MAX_COMMAND_OUTPUT_BYTES,
                        },
                    },
                    "required": ["command"],
                    "additionalProperties": false,
                },
            })
        );
    }

    #[test]
    fn git_pathspecs_force_literal_matching() {
        assert_eq!(git_pathspec("README.md").unwrap(), ":(literal)README.md");
        assert_eq!(git_pathspec(":(top)*").unwrap(), ":(literal):(top)*");
    }

    #[tokio::test]
    async fn executes_read_file_and_list_dir() {
        let temp = std::env::temp_dir().join(format!("rust-agent-tools-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src")).unwrap();
        fs::write(temp.join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let read = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"src/main.rs"}"#.to_string(),
            })
            .await;
        let range = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_range".to_string(),
                name: READ_FILE_RANGE_TOOL.to_string(),
                arguments: r#"{"path":"src/main.rs","offset":3,"max_bytes":10}"#.to_string(),
            })
            .await;
        let list = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_list".to_string(),
                name: LIST_DIR_TOOL.to_string(),
                arguments: r#"{"path":"."}"#.to_string(),
            })
            .await;

        assert!(read.success);
        assert_eq!(read.output, "fn main() {}\n");
        assert!(range.success);
        assert_eq!(range.output, "main() {}\n");
        assert!(list.success);
        assert_eq!(list.output, "src/");

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn read_file_supports_line_pagination() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-read-lines-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let read = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read_lines".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"notes.txt","offset":2,"limit":2}"#.to_string(),
            })
            .await;

        assert!(read.success, "{}", read.output);
        assert!(read.output.contains("<path>notes.txt</path>"));
        assert!(read.output.contains("2: beta"));
        assert!(read.output.contains("3: gamma"));
        assert!(!read.output.contains("1: alpha"));
        assert!(read.output.contains("Use offset=4 to continue."));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn read_file_line_pages_do_not_decode_after_limit() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-read-lines-invalid-tail-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("notes.txt"), b"alpha\n\xff").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let read = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read_lines".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"notes.txt","offset":1,"limit":1}"#.to_string(),
            })
            .await;

        assert!(read.success, "{}", read.output);
        assert!(read.output.contains("1: alpha"), "{}", read.output);
        assert!(read.output.contains("Use offset=2 to continue."));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn read_file_line_pages_cap_long_lines_before_string_allocation() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-read-lines-long-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let long = "x".repeat(MAX_READ_LINE_BYTES + 1024);
        fs::write(temp.join("notes.txt"), format!("{long}\ntarget\n")).unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let first = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read_long_line".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"notes.txt","offset":1,"limit":1}"#.to_string(),
            })
            .await;
        let second = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read_after_long_line".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"notes.txt","offset":2,"limit":1}"#.to_string(),
            })
            .await;

        assert!(first.success, "{}", first.output);
        assert!(
            first.output.contains(READ_LINE_TRUNCATION_SUFFIX),
            "{}",
            first.output
        );
        assert!(first.output.len() < MAX_READ_LINE_BYTES + 512);
        assert!(second.success, "{}", second.output);
        assert!(second.output.contains("2: target"), "{}", second.output);

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn list_dir_supports_pagination_and_depth() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-list-page-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src").join("bin")).unwrap();
        fs::write(temp.join("Cargo.toml"), "").unwrap();
        fs::write(temp.join("src").join("lib.rs"), "").unwrap();
        fs::write(temp.join("src").join("bin").join("cli.rs"), "").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let page = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_list_page".to_string(),
                name: LIST_DIR_TOOL.to_string(),
                arguments: r#"{"path":".","offset":2,"limit":2,"depth":3}"#.to_string(),
            })
            .await;

        assert!(page.success, "{}", page.output);
        assert!(!page.output.contains("Cargo.toml"), "{}", page.output);
        assert!(page.output.contains("src/"), "{}", page.output);
        assert!(page.output.contains("src/bin/"), "{}", page.output);
        assert!(
            page.output.contains("use offset=4") || page.output.contains("src/bin/cli.rs"),
            "{}",
            page.output
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn executes_shell_commands_and_allows_git() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-exec-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src")).unwrap();
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);

        let command = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_exec".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({
                    "command": "printf '%s' \"$PWD\"",
                    "cwd": "src",
                    "timeout_ms": 5000,
                })
                .to_string(),
            })
            .await;
        let git_command = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_git_allowed".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({ "command": "git --version" }).to_string(),
            })
            .await;
        let failed = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_exec_failed".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({ "command": "printf fail >&2; exit 7" }).to_string(),
            })
            .await;
        let capped = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_exec_capped".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({
                    "command": format!("yes x | head -c {}", MAX_COMMAND_TOTAL_OUTPUT_BYTES + 1024),
                    "max_output_bytes": 1024,
                })
                .to_string(),
            })
            .await;
        let tail_preview = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_exec_tail_preview".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({
                    "command": "printf 'start\\n'; for i in $(seq 1 400); do printf 'mid-%03d\\n' \"$i\"; done; printf 'end\\n'",
                    "max_output_bytes": 128,
                })
                .to_string(),
            })
            .await;

        assert!(command.success, "{}", command.output);
        assert!(command.output.contains("src"), "{}", command.output);
        assert!(git_command.success, "{}", git_command.output);
        assert!(
            git_command.output.contains("git version"),
            "{}",
            git_command.output
        );
        assert!(!failed.success);
        assert!(
            failed.output.contains("status: exit 7"),
            "{}",
            failed.output
        );
        assert!(failed.output.contains("fail"), "{}", failed.output);
        assert!(!failed.output.contains("Tool error"), "{}", failed.output);
        assert!(!capped.success);
        assert!(
            capped.output.contains("output exceeded"),
            "{}",
            capped.output
        );
        let artifact_marker = "full output saved to ";
        let artifact_start = capped
            .output
            .find(artifact_marker)
            .expect("capped output should mention an artifact")
            + artifact_marker.len();
        let artifact_end = capped.output[artifact_start..]
            .find(']')
            .expect("artifact notice should be bracketed")
            + artifact_start;
        let artifact_path = temp.join(&capped.output[artifact_start..artifact_end]);
        assert!(artifact_path.exists(), "{}", artifact_path.display());
        assert!(
            fs::metadata(&artifact_path).unwrap().len() > 1024,
            "{}",
            artifact_path.display()
        );
        assert!(tail_preview.success, "{}", tail_preview.output);
        assert!(
            tail_preview.output.contains("showing last"),
            "{}",
            tail_preview.output
        );
        assert!(
            tail_preview.output.contains("end"),
            "{}",
            tail_preview.output
        );
        assert!(
            !tail_preview.output.contains("mid-001"),
            "{}",
            tail_preview.output
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn cancels_running_shell_command() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-cancel-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);
        let cancellation = TurnCancellation::new();

        let running = tokio::spawn({
            let registry = registry.clone();
            let cancellation = cancellation.clone();
            async move {
                registry
                    .execute_ref_with_cancellation(
                        &ModelToolCall {
                            item_id: None,
                            call_id: "call_exec_cancel".to_string(),
                            name: EXEC_COMMAND_TOOL.to_string(),
                            arguments: json!({
                                "command": "sleep 5",
                                "timeout_ms": 300_000,
                            })
                            .to_string(),
                        },
                        &cancellation,
                    )
                    .await
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
        let execution = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("cancelled command should finish promptly")
            .expect("command task should not panic");

        assert!(!execution.success);
        assert!(
            execution.output.contains("status: cancelled"),
            "{}",
            execution.output
        );
        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn executes_dedicated_git_wrappers() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-git-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        run_test_git(&temp, ["init"]);
        run_test_git(&temp, ["config", "user.email", "agent@example.com"]);
        run_test_git(&temp, ["config", "user.name", "Rust Agent"]);
        fs::write(temp.join("README.md"), "old\n").unwrap();
        run_test_git(&temp, ["add", "README.md"]);
        run_test_git(&temp, ["commit", "-m", "Initial commit"]);
        fs::write(temp.join("README.md"), "new\n").unwrap();
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);

        let status = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_status".to_string(),
                name: GIT_STATUS_TOOL.to_string(),
                arguments: "{}".to_string(),
            })
            .await;
        let diff = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_diff".to_string(),
                name: GIT_DIFF_TOOL.to_string(),
                arguments: json!({ "path": "README.md" }).to_string(),
            })
            .await;
        let show = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_query_show".to_string(),
                name: GIT_QUERY_TOOL.to_string(),
                arguments: json!({
                    "command": "show",
                    "args": ["HEAD:README.md"],
                })
                .to_string(),
            })
            .await;
        let branch_delete = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_query_branch_delete".to_string(),
                name: GIT_QUERY_TOOL.to_string(),
                arguments: json!({
                    "command": "branch",
                    "args": ["-D", "main"],
                })
                .to_string(),
            })
            .await;
        let add = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_add".to_string(),
                name: GIT_ADD_TOOL.to_string(),
                arguments: json!({ "paths": ["README.md"] }).to_string(),
            })
            .await;
        let staged_status = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_status_staged".to_string(),
                name: GIT_STATUS_TOOL.to_string(),
                arguments: "{}".to_string(),
            })
            .await;
        let restore_staged = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_restore_staged".to_string(),
                name: GIT_RESTORE_TOOL.to_string(),
                arguments: json!({
                    "paths": ["README.md"],
                    "staged": true,
                })
                .to_string(),
            })
            .await;
        let unstaged_status = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_status_unstaged".to_string(),
                name: GIT_STATUS_TOOL.to_string(),
                arguments: "{}".to_string(),
            })
            .await;
        let commit = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_commit".to_string(),
                name: GIT_COMMIT_TOOL.to_string(),
                arguments: json!({
                    "message": "Update readme",
                    "paths": ["README.md"],
                })
                .to_string(),
            })
            .await;
        let log = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_log".to_string(),
                name: GIT_LOG_TOOL.to_string(),
                arguments: json!({ "max_count": 1 }).to_string(),
            })
            .await;
        fs::write(temp.join("README.md"), "discard\n").unwrap();
        let restore_worktree = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_restore_worktree".to_string(),
                name: GIT_RESTORE_TOOL.to_string(),
                arguments: json!({ "paths": ["README.md"] }).to_string(),
            })
            .await;

        assert!(status.success, "{}", status.output);
        assert!(status.output.contains(" M README.md"), "{}", status.output);
        assert!(diff.success, "{}", diff.output);
        assert!(diff.output.contains("-old"), "{}", diff.output);
        assert!(diff.output.contains("+new"), "{}", diff.output);
        assert!(show.success, "{}", show.output);
        assert!(show.output.contains("old"), "{}", show.output);
        assert!(!branch_delete.success, "{}", branch_delete.output);
        assert!(
            branch_delete
                .output
                .contains("only supports --show-current"),
            "{}",
            branch_delete.output
        );
        assert!(add.success, "{}", add.output);
        assert!(staged_status.success, "{}", staged_status.output);
        assert!(
            staged_status.output.contains("M  README.md"),
            "{}",
            staged_status.output
        );
        assert!(restore_staged.success, "{}", restore_staged.output);
        assert!(unstaged_status.success, "{}", unstaged_status.output);
        assert!(
            unstaged_status.output.contains(" M README.md"),
            "{}",
            unstaged_status.output
        );
        assert!(commit.success, "{}", commit.output);
        assert!(commit.output.contains("git commit"), "{}", commit.output);
        assert!(log.success, "{}", log.output);
        assert!(log.output.contains("Update readme"), "{}", log.output);
        assert!(restore_worktree.success, "{}", restore_worktree.output);
        assert_eq!(fs::read_to_string(temp.join("README.md")).unwrap(), "new\n");

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn git_wrappers_treat_magic_pathspecs_as_literal() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-git-magic-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        run_test_git(&temp, ["init"]);
        run_test_git(&temp, ["config", "user.email", "agent@example.com"]);
        run_test_git(&temp, ["config", "user.name", "Rust Agent"]);
        fs::write(temp.join("a.txt"), "old a\n").unwrap();
        fs::write(temp.join("b.txt"), "old b\n").unwrap();
        run_test_git(&temp, ["add", "a.txt", "b.txt"]);
        run_test_git(&temp, ["commit", "-m", "Initial commit"]);
        fs::write(temp.join("a.txt"), "new a\n").unwrap();
        fs::write(temp.join("b.txt"), "new b\n").unwrap();

        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);
        let restore = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_restore_magic".to_string(),
                name: GIT_RESTORE_TOOL.to_string(),
                arguments: json!({ "paths": [":(top)*"] }).to_string(),
            })
            .await;
        let commit = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_commit_magic".to_string(),
                name: GIT_COMMIT_TOOL.to_string(),
                arguments: json!({
                    "message": "Try magic pathspec",
                    "paths": [":(top)*"],
                })
                .to_string(),
            })
            .await;

        assert!(!restore.success, "{}", restore.output);
        assert_eq!(fs::read_to_string(temp.join("a.txt")).unwrap(), "new a\n");
        assert_eq!(fs::read_to_string(temp.join("b.txt")).unwrap(), "new b\n");
        assert!(!commit.success, "{}", commit.output);

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn applies_workspace_patch_changes() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-patch-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src")).unwrap();
        fs::write(
            temp.join("src").join("lib.rs"),
            "pub fn value() -> &'static str {\n    \"old\"\n}\n",
        )
        .unwrap();
        fs::write(temp.join("obsolete.rs"), "delete me\n").unwrap();

        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: src/lib.rs\n",
            "@@\n",
            " pub fn value() -> &'static str {\n",
            "-    \"old\"\n",
            "+    \"new\"\n",
            " }\n",
            "*** Add File: src/new.rs\n",
            "+pub fn created() {}\n",
            "*** Delete File: obsolete.rs\n",
            "*** End Patch\n",
        );
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceWrite);

        let result = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_patch".to_string(),
                name: APPLY_PATCH_TOOL.to_string(),
                arguments: json!({ "patch": patch }).to_string(),
            })
            .await;

        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("updated src/lib.rs"));
        assert!(result.output.contains("added src/new.rs"));
        assert!(result.output.contains("deleted obsolete.rs"));
        assert_eq!(
            fs::read_to_string(temp.join("src").join("lib.rs")).unwrap(),
            "pub fn value() -> &'static str {\n    \"new\"\n}\n"
        );
        assert_eq!(
            fs::read_to_string(temp.join("src").join("new.rs")).unwrap(),
            "pub fn created() {}\n"
        );
        assert!(!temp.join("obsolete.rs").exists());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn apply_patch_rolls_back_applied_changes_when_later_commit_fails() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-patch-rollback-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let temp = temp.canonicalize().unwrap();
        fs::write(temp.join("a.txt"), "old a\n").unwrap();
        fs::write(temp.join("b.txt"), "old b\n").unwrap();
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: a.txt\n",
            "@@\n",
            "-old a\n",
            "+new a\n",
            "*** Update File: b.txt\n",
            "@@\n",
            "-old b\n",
            "+new b\n",
            "*** End Patch\n",
        );
        let changes = prepare_patch_changes(&temp, ParsedPatch::parse(patch).unwrap())
            .await
            .unwrap();
        fs::remove_file(temp.join("b.txt")).unwrap();
        fs::create_dir(temp.join("b.txt")).unwrap();

        let error = apply_prepared_changes(&temp, &changes)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("failed to replace"), "{error}");
        assert_eq!(fs::read_to_string(temp.join("a.txt")).unwrap(), "old a\n");
        assert!(fs::metadata(temp.join("b.txt")).unwrap().is_dir());
        let leaked_temp = fs::read_dir(&temp)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .find(|name| name.contains(".rust-agent-"));
        assert_eq!(leaked_temp, None);

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn apply_patch_is_disabled_by_default() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-patch-disabled-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("lib.rs"), "fn old() {}\n").unwrap();
        let registry = ToolRegistry::for_root(&temp);

        let result = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_patch".to_string(),
                name: APPLY_PATCH_TOOL.to_string(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Delete File: lib.rs\n*** End Patch\n"
                })
                .to_string(),
            })
            .await;

        assert!(!result.success);
        assert!(result.output.contains("tool is not enabled"));
        assert_eq!(
            fs::read_to_string(temp.join("lib.rs")).unwrap(),
            "fn old() {}\n"
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn apply_patch_rejects_escaping_paths_before_writing() {
        let parent = std::env::temp_dir().join(format!(
            "rust-agent-tools-patch-escape-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let root = parent.join("workspace");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("lib.rs"), "fn value() {}\n").unwrap();
        fs::write(parent.join("outside.rs"), "missing\n").unwrap();
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Add File: created.rs\n",
            "+created\n",
            "*** Update File: ../outside.rs\n",
            "@@\n",
            "-missing\n",
            "+changed\n",
            "*** End Patch\n",
        );
        let registry = ToolRegistry::for_root_with_policy(&root, ToolPolicy::WorkspaceWrite);

        let result = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_patch".to_string(),
                name: APPLY_PATCH_TOOL.to_string(),
                arguments: json!({ "patch": patch }).to_string(),
            })
            .await;

        assert!(!result.success);
        assert!(result.output.contains("path escapes workspace"));
        assert!(!root.join("created.rs").exists());
        assert_eq!(
            fs::read_to_string(parent.join("outside.rs")).unwrap(),
            "missing\n"
        );

        fs::remove_dir_all(&parent).unwrap();
    }

    #[tokio::test]
    async fn apply_patch_rejects_non_ascii_hunk_line_without_panic() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-patch-hunk-prefix-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("lib.rs"), "fn value() {}\n").unwrap();
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: lib.rs\n",
            "@@\n",
            "é malformed\n",
            "*** End Patch\n",
        );
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceWrite);

        let result = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_patch".to_string(),
                name: APPLY_PATCH_TOOL.to_string(),
                arguments: json!({ "patch": patch }).to_string(),
            })
            .await;

        assert!(!result.success);
        assert!(result.output.contains("hunk lines must start"));
        assert_eq!(
            fs::read_to_string(temp.join("lib.rs")).unwrap(),
            "fn value() {}\n"
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn executes_fff_search_tools() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-search-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src")).unwrap();
        fs::write(
            temp.join("src").join("main.rs"),
            "fn main() {\n    println!(\"special_needle\");\n}\n",
        )
        .unwrap();
        fs::write(temp.join("src").join("lib.rs"), "pub fn helper() {}\n").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        let files = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_search_files".to_string(),
                name: SEARCH_FILES_TOOL.to_string(),
                arguments: r#"{"query":"main","limit":5}"#.to_string(),
            })
            .await;
        let text = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_search_text".to_string(),
                name: SEARCH_TEXT_TOOL.to_string(),
                arguments: r#"{"query":"special_needle","limit":5}"#.to_string(),
            })
            .await;

        assert!(files.success, "{}", files.output);
        assert!(files.output.contains("src/main.rs"), "{}", files.output);
        assert!(text.success, "{}", text.output);
        assert!(text.output.contains("src/main.rs:2:"), "{}", text.output);
        assert!(text.output.contains("special_needle"), "{}", text.output);

        drop(registry);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn initializes_fff_search_index_before_first_search() {
        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-warmup-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("src")).unwrap();
        fs::write(temp.join("src").join("lib.rs"), "pub fn warm_target() {}\n").unwrap();

        let registry = ToolRegistry::for_root(&temp);
        assert!(registry.search.state.lock().unwrap().is_none());

        registry
            .spawn_search_index_warmup()
            .await
            .expect("index initialization task should not panic")
            .expect("index initialization should finish");

        assert!(registry.search.state.lock().unwrap().is_some());
        let files = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_search_files".to_string(),
                name: SEARCH_FILES_TOOL.to_string(),
                arguments: r#"{"query":"lib","limit":5}"#.to_string(),
            })
            .await;

        assert!(files.success, "{}", files.output);
        assert!(files.output.contains("src/lib.rs"), "{}", files.output);

        drop(registry);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn read_file_returns_capped_output_for_large_files() {
        let temp =
            std::env::temp_dir().join(format!("rust-agent-tools-large-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("large.log"), vec![b'a'; MAX_FILE_BYTES + 1024]).unwrap();
        let registry = ToolRegistry::for_root(&temp);

        let read = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"large.log"}"#.to_string(),
            })
            .await;

        assert!(read.success);
        assert_eq!(
            read.output.len(),
            MAX_FILE_BYTES + FILE_TRUNCATION_MARKER.len()
        );
        assert!(read.output.as_bytes()[..MAX_FILE_BYTES]
            .iter()
            .all(|byte| *byte == b'a'));
        assert!(read.output.ends_with(FILE_TRUNCATION_MARKER));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    #[ignore = "release-mode capped read_file benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_read_file_capped_large_file() {
        const FILE_BYTES: u64 = 128 * 1024 * 1024;
        const SAMPLES: usize = 15;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-bench-large-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let file = fs::File::create(temp.join("large.log")).unwrap();
        file.set_len(FILE_BYTES).unwrap();
        let registry = ToolRegistry::for_root(&temp);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let read = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_read".to_string(),
                    name: READ_FILE_TOOL.to_string(),
                    arguments: r#"{"path":"large.log"}"#.to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(read.success);
            assert!(read.output.ends_with(FILE_TRUNCATION_MARKER));
            output_bytes = read.output.len();
            std::hint::black_box(&read.output);
            samples.push(elapsed);
        }

        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "read_file_capped_large_file file_bytes={FILE_BYTES} output_bytes={output_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode read_file_range benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_read_file_range_large_file() {
        const FILE_BYTES: u64 = 128 * 1024 * 1024;
        const RANGE_BYTES: usize = 4096;
        const SAMPLES: usize = 15;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-bench-range-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let file = fs::File::create(temp.join("large.log")).unwrap();
        file.set_len(FILE_BYTES).unwrap();
        let registry = ToolRegistry::for_root(&temp);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let read = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_range".to_string(),
                    name: READ_FILE_RANGE_TOOL.to_string(),
                    arguments: json!({
                        "path": "large.log",
                        "offset": FILE_BYTES - RANGE_BYTES as u64,
                        "max_bytes": RANGE_BYTES,
                    })
                    .to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(read.success);
            output_bytes = read.output.len();
            assert_eq!(output_bytes, RANGE_BYTES);
            std::hint::black_box(&read.output);
            samples.push(elapsed);
        }

        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "read_file_range_large_file file_bytes={FILE_BYTES} range_bytes={RANGE_BYTES} output_bytes={output_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode large directory benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_list_dir_large_directory() {
        const FILES: usize = 10_000;
        const SAMPLES: usize = 10;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-list-bench-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        for index in 0..FILES {
            fs::write(temp.join(format!("file-{index:05}.txt")), "").unwrap();
        }
        let registry = ToolRegistry::for_root(&temp);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let list = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_list".to_string(),
                    name: LIST_DIR_TOOL.to_string(),
                    arguments: r#"{"path":"."}"#.to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(list.success, "{}", list.output);
            assert!(list.output.contains("[truncated"), "{}", list.output);
            output_bytes = list.output.len();
            std::hint::black_box(&list.output);
            samples.push(elapsed);
        }

        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "list_dir_large_directory files={FILES} output_bytes={output_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode command output benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_exec_command_large_output() {
        const COMMAND_BYTES: usize = 8 * 1024 * 1024;
        const SAMPLES: usize = 10;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-command-bench-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceExec);
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;
        let mut successes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let command = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_exec".to_string(),
                    name: EXEC_COMMAND_TOOL.to_string(),
                    arguments: json!({
                        "command": format!("yes x | head -c {COMMAND_BYTES}"),
                        "timeout_ms": 30_000,
                        "max_output_bytes": DEFAULT_COMMAND_OUTPUT_BYTES,
                    })
                    .to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            if command.success {
                successes += 1;
            }
            assert!(command.output.contains("stdout"), "{}", command.output);
            output_bytes = command.output.len();
            std::hint::black_box(&command.output);
            samples.push(elapsed);
        }

        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "exec_command_large_output command_bytes={COMMAND_BYTES} retained_output_bytes={output_bytes} successes={successes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode FFF search benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_fff_search_current_repo() {
        const SAMPLES: usize = 15;

        let root = std::env::current_dir().unwrap();
        let registry = ToolRegistry::for_root(root);

        let cold_started = Instant::now();
        let cold = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "cold_search_files".to_string(),
                name: SEARCH_FILES_TOOL.to_string(),
                arguments: r#"{"query":"tools","limit":20}"#.to_string(),
            })
            .await;
        let cold_elapsed = cold_started.elapsed();
        assert!(cold.success, "{}", cold.output);
        assert!(cold.output.contains("src/tools.rs"), "{}", cold.output);

        let mut file_samples = Vec::with_capacity(SAMPLES);
        let mut text_samples = Vec::with_capacity(SAMPLES);
        let mut file_output_bytes = 0usize;
        let mut text_output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let search = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "warm_search_files".to_string(),
                    name: SEARCH_FILES_TOOL.to_string(),
                    arguments: r#"{"query":"tools","limit":20}"#.to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(search.success, "{}", search.output);
            assert!(search.output.contains("src/tools.rs"), "{}", search.output);
            file_output_bytes = search.output.len();
            std::hint::black_box(&search.output);
            file_samples.push(elapsed);
        }

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let search = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "warm_search_text".to_string(),
                    name: SEARCH_TEXT_TOOL.to_string(),
                    arguments: r#"{"query":"ToolRegistry","limit":20}"#.to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(search.success, "{}", search.output);
            assert!(search.output.contains("ToolRegistry"), "{}", search.output);
            text_output_bytes = search.output.len();
            std::hint::black_box(&search.output);
            text_samples.push(elapsed);
        }

        let file = DurationSummary::from_samples(&mut file_samples);
        let text = DurationSummary::from_samples(&mut text_samples);
        println!(
            "fff_search_current_repo samples={SAMPLES} cold_file_search_ms={:.3} warm_file_min_ms={:.3} warm_file_median_ms={:.3} warm_file_max_ms={:.3} warm_file_output_bytes={file_output_bytes} warm_text_min_ms={:.3} warm_text_median_ms={:.3} warm_text_max_ms={:.3} warm_text_output_bytes={text_output_bytes}",
            cold_elapsed.as_secs_f64() * 1000.0,
            file.min_ms(),
            file.median_ms(),
            file.max_ms(),
            text.min_ms(),
            text.median_ms(),
            text.max_ms(),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "release-mode parallel FFF search benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_fff_parallel_search_current_repo() {
        const SAMPLES: usize = 5;
        const SEARCHES: usize = 16;

        let root = std::env::current_dir().unwrap();
        let registry = ToolRegistry::for_root(root);

        let warm = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "warm_search_files".to_string(),
                name: SEARCH_FILES_TOOL.to_string(),
                arguments: r#"{"query":"src tools","limit":20}"#.to_string(),
            })
            .await;
        assert!(warm.success, "{}", warm.output);

        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;
        for sample in 0..SAMPLES {
            let started = Instant::now();
            let executions = futures_util::future::join_all((0..SEARCHES).map(|index| {
                let registry = registry.clone();
                let name = if index % 2 == 0 {
                    SEARCH_FILES_TOOL
                } else {
                    SEARCH_TEXT_TOOL
                };
                let arguments = if index % 2 == 0 {
                    r#"{"query":"src tools","limit":20}"#
                } else {
                    r#"{"query":"ToolRegistry","limit":20}"#
                };
                async move {
                    registry
                        .execute(ModelToolCall {
                            item_id: None,
                            call_id: format!("parallel_search_{sample}_{index}"),
                            name: name.to_string(),
                            arguments: arguments.to_string(),
                        })
                        .await
                }
            }))
            .await;
            samples.push(started.elapsed());

            for execution in executions {
                assert!(execution.success, "{}", execution.output);
                output_bytes += execution.output.len();
            }
        }

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "fff_parallel_search_current_repo searches_per_sample={SEARCHES} samples={SAMPLES} output_bytes={output_bytes} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode text search output benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_search_text_large_line_output() {
        const LINE_BYTES: usize = 1024 * 1024;
        const SAMPLES: usize = 10;

        let temp = std::env::temp_dir().join(format!(
            "rust-agent-tools-search-output-bench-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("large-line.txt"),
            format!("needle {}\n", "x".repeat(LINE_BYTES)),
        )
        .unwrap();

        let registry = ToolRegistry::for_root(&temp);
        registry
            .spawn_search_index_warmup()
            .await
            .expect("index warmup task should not panic")
            .expect("index warmup should succeed");
        let mut samples = Vec::with_capacity(SAMPLES);
        let mut output_bytes = 0usize;

        for _ in 0..SAMPLES {
            let started = Instant::now();
            let search = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_search_text".to_string(),
                    name: SEARCH_TEXT_TOOL.to_string(),
                    arguments: r#"{"query":"needle","limit":1}"#.to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(search.success, "{}", search.output);
            assert!(search.output.contains("needle"), "{}", search.output);
            output_bytes = search.output.len();
            std::hint::black_box(&search.output);
            samples.push(elapsed);
        }

        drop(registry);
        fs::remove_dir_all(&temp).unwrap();

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "search_text_large_line_output line_bytes={LINE_BYTES} output_bytes={output_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    #[ignore = "release-mode patch planning benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_apply_patch_many_large_files() {
        const FILES: usize = 24;
        const FILE_BYTES: usize = 128 * 1024;
        const SAMPLES: usize = 8;

        let mut samples = Vec::with_capacity(SAMPLES);
        let changed_bytes = FILES * FILE_BYTES;
        for sample in 0..SAMPLES {
            let temp = std::env::temp_dir().join(format!(
                "rust-agent-tools-patch-bench-{}-{}-{sample}",
                std::process::id(),
                unique_nanos()
            ));
            let _ = fs::remove_dir_all(&temp);
            fs::create_dir_all(&temp).unwrap();
            for index in 0..FILES {
                let content = format!("old-{index}\n{}", "x".repeat(FILE_BYTES - 16));
                fs::write(temp.join(format!("file-{index:02}.txt")), content).unwrap();
            }

            let registry = ToolRegistry::for_root_with_policy(&temp, ToolPolicy::WorkspaceWrite);
            let patch = patch_many_large_files(FILES);
            let started = Instant::now();
            let result = registry
                .execute(ModelToolCall {
                    item_id: None,
                    call_id: "call_patch".to_string(),
                    name: APPLY_PATCH_TOOL.to_string(),
                    arguments: json!({ "patch": patch }).to_string(),
                })
                .await;
            let elapsed = started.elapsed();

            assert!(result.success, "{}", result.output);
            std::hint::black_box(&result.output);
            samples.push(elapsed);
            fs::remove_dir_all(&temp).unwrap();
        }

        let summary = DurationSummary::from_samples(&mut samples);
        println!(
            "apply_patch_many_large_files files={FILES} file_bytes={FILE_BYTES} changed_bytes={changed_bytes} samples={SAMPLES} min_ms={:.3} median_ms={:.3} max_ms={:.3}",
            summary.min_ms(),
            summary.median_ms(),
            summary.max_ms(),
        );
    }

    #[tokio::test]
    async fn rejects_paths_that_escape_workspace() {
        let parent =
            std::env::temp_dir().join(format!("rust-agent-tools-parent-{}", std::process::id()));
        let root = parent.join("workspace");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        fs::write(parent.join("Cargo.toml"), "[package]\n").unwrap();
        let registry = ToolRegistry::for_root(&root);

        let result = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_read".to_string(),
                name: READ_FILE_TOOL.to_string(),
                arguments: r#"{"path":"../Cargo.toml"}"#.to_string(),
            })
            .await;

        assert!(!result.success);
        assert!(result.output.contains("path escapes workspace"));
        fs::remove_dir_all(&parent).unwrap();
    }

    fn unique_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn patch_many_large_files(files: usize) -> String {
        let mut patch = String::from("*** Begin Patch\n");
        for index in 0..files {
            patch.push_str(&format!("*** Update File: file-{index:02}.txt\n"));
            patch.push_str("@@\n");
            patch.push_str(&format!("-old-{index}\n"));
            patch.push_str(&format!("+new-{index}\n"));
        }
        patch.push_str("*** End Patch\n");
        patch
    }

    fn run_test_git<const N: usize>(cwd: &Path, args: [&str; N]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git should be available for git wrapper tests");
        assert!(
            output.status.success(),
            "git failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
