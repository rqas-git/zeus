//! Built-in tool registry and execution.

use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

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
use serde::ser::SerializeMap;
use serde::ser::SerializeStruct;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent_loop::ModelToolCall;

const READ_FILE_TOOL: &str = "read_file";
const LIST_DIR_TOOL: &str = "list_dir";
const SEARCH_FILES_TOOL: &str = "search_files";
const SEARCH_TEXT_TOOL: &str = "search_text";
const APPLY_PATCH_TOOL: &str = "apply_patch";
const EXEC_COMMAND_TOOL: &str = "exec_command";
const GIT_STATUS_TOOL: &str = "git_status";
const GIT_DIFF_TOOL: &str = "git_diff";
const GIT_LOG_TOOL: &str = "git_log";
const GIT_COMMIT_TOOL: &str = "git_commit";
const MAX_FILE_BYTES: usize = 64 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n[truncated: file exceeds 65536 bytes]";
const MAX_DIR_ENTRIES: usize = 200;
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_PATCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 30_000;
const MAX_COMMAND_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;
const DEFAULT_GIT_LOG_COUNT: usize = 10;
const MAX_GIT_LOG_COUNT: usize = 50;
const DEFAULT_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_OFFSET: usize = 100_000;
const DEFAULT_TEXT_SEARCH_TIMEOUT_MS: u64 = 250;
const MAX_TEXT_CONTEXT_LINES: usize = 3;
const FFF_SCAN_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);

const READ_ONLY_TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: READ_FILE_TOOL,
        description: "Read a UTF-8 text file from the current workspace.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative path to the file to read.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: LIST_DIR_TOOL,
        description: "List files and directories under a workspace directory.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative directory path. Use . for the workspace root.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: SEARCH_FILES_TOOL,
        description: "Fuzzy-search indexed workspace files and directories by path.",
        parameters: ToolParameters::SearchFiles,
        supports_parallel: false,
    },
    ToolSpec {
        name: SEARCH_TEXT_TOOL,
        description:
            "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
        parameters: ToolParameters::SearchText,
        supports_parallel: false,
    },
];

const WORKSPACE_WRITE_TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: READ_FILE_TOOL,
        description: "Read a UTF-8 text file from the current workspace.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative path to the file to read.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: LIST_DIR_TOOL,
        description: "List files and directories under a workspace directory.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative directory path. Use . for the workspace root.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: SEARCH_FILES_TOOL,
        description: "Fuzzy-search indexed workspace files and directories by path.",
        parameters: ToolParameters::SearchFiles,
        supports_parallel: false,
    },
    ToolSpec {
        name: SEARCH_TEXT_TOOL,
        description:
            "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
        parameters: ToolParameters::SearchText,
        supports_parallel: false,
    },
    ToolSpec {
        name: APPLY_PATCH_TOOL,
        description: "Apply a workspace-confined patch that adds, updates, or deletes UTF-8 files.",
        parameters: ToolParameters::ApplyPatch,
        supports_parallel: false,
    },
];

const WORKSPACE_EXEC_TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: READ_FILE_TOOL,
        description: "Read a UTF-8 text file from the current workspace.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative path to the file to read.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: LIST_DIR_TOOL,
        description: "List files and directories under a workspace directory.",
        parameters: ToolParameters::Path {
            description: "Workspace-relative directory path. Use . for the workspace root.",
        },
        supports_parallel: true,
    },
    ToolSpec {
        name: SEARCH_FILES_TOOL,
        description: "Fuzzy-search indexed workspace files and directories by path.",
        parameters: ToolParameters::SearchFiles,
        supports_parallel: false,
    },
    ToolSpec {
        name: SEARCH_TEXT_TOOL,
        description:
            "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
        parameters: ToolParameters::SearchText,
        supports_parallel: false,
    },
    ToolSpec {
        name: APPLY_PATCH_TOOL,
        description: "Apply a workspace-confined patch that adds, updates, or deletes UTF-8 files.",
        parameters: ToolParameters::ApplyPatch,
        supports_parallel: false,
    },
    ToolSpec {
        name: EXEC_COMMAND_TOOL,
        description:
            "Execute a non-git shell command from the workspace root and return bounded output.",
        parameters: ToolParameters::ExecCommand,
        supports_parallel: false,
    },
    ToolSpec {
        name: GIT_STATUS_TOOL,
        description: "Show concise git worktree status for the workspace repository.",
        parameters: ToolParameters::NoArgs,
        supports_parallel: false,
    },
    ToolSpec {
        name: GIT_DIFF_TOOL,
        description: "Show bounded git diff output for unstaged or staged workspace changes.",
        parameters: ToolParameters::GitDiff,
        supports_parallel: false,
    },
    ToolSpec {
        name: GIT_LOG_TOOL,
        description: "Show recent git commits for the workspace repository.",
        parameters: ToolParameters::GitLog,
        supports_parallel: false,
    },
    ToolSpec {
        name: GIT_COMMIT_TOOL,
        description: "Create an atomic git commit for explicit workspace-relative paths.",
        parameters: ToolParameters::GitCommit,
        supports_parallel: false,
    },
];

/// Permission set controlling which built-in tools are exposed and executable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToolPolicy {
    #[default]
    ReadOnly,
    WorkspaceWrite,
    WorkspaceExec,
}

impl ToolPolicy {
    const fn specs(self) -> &'static [ToolSpec] {
        match self {
            Self::ReadOnly => READ_ONLY_TOOL_SPECS,
            Self::WorkspaceWrite => WORKSPACE_WRITE_TOOL_SPECS,
            Self::WorkspaceExec => WORKSPACE_EXEC_TOOL_SPECS,
        }
    }
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
    Path { description: &'static str },
    SearchFiles,
    SearchText,
    ApplyPatch,
    ExecCommand,
    NoArgs,
    GitDiff,
    GitLog,
    GitCommit,
}

impl ToolParameters {
    const fn cache_key(self) -> &'static str {
        match self {
            Self::Path { .. } => "path:string:required:no_additional_properties",
            Self::SearchFiles => "search_files:query:string:required:limit:integer:offset:integer",
            Self::SearchText => {
                "search_text:query:string:required:mode:string:limit:integer:file_offset:integer:context:integer"
            }
            Self::ApplyPatch => "apply_patch:patch:string:required:no_additional_properties",
            Self::ExecCommand => {
                "exec_command:command:string:required:cwd:string:timeout_ms:integer:max_output_bytes:integer"
            }
            Self::NoArgs => "no_args:no_additional_properties",
            Self::GitDiff => "git_diff:staged:boolean:path:string:max_output_bytes:integer",
            Self::GitLog => "git_log:max_count:integer",
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
            Self::Path { description } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "object")?;
                map.serialize_entry("properties", &PathProperties { description })?;
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

struct PathProperties {
    description: &'static str,
}

impl Serialize for PathProperties {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(
            "path",
            &StringProperty {
                description: self.description,
            },
        )?;
        map.end()
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
                description:
                    "Shell command to execute. Direct git commands are rejected; use git_* tools.",
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
    policy: ToolPolicy,
    root: Arc<PathBuf>,
    search: FffSearchIndex,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::for_root(root)
    }
}

impl ToolRegistry {
    /// Creates a tool registry for the current directory with explicit permissions.
    pub(crate) fn with_policy(policy: ToolPolicy) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::for_root_with_policy(root, policy)
    }

    /// Creates a tool registry rooted at `root`.
    pub(crate) fn for_root(root: impl Into<PathBuf>) -> Self {
        Self::for_root_with_policy(root, ToolPolicy::ReadOnly)
    }

    /// Creates a tool registry rooted at `root` with explicit permissions.
    pub(crate) fn for_root_with_policy(root: impl Into<PathBuf>, policy: ToolPolicy) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self {
            policy,
            search: FffSearchIndex::new(root.clone()),
            root: Arc::new(root),
        }
    }

    /// Initializes the FFF search index on a blocking worker.
    pub(crate) fn spawn_search_index_warmup(&self) -> tokio::task::JoinHandle<Result<()>> {
        let search = self.search.clone();
        tokio::task::spawn_blocking(move || search.warm())
    }

    /// Returns the stable model-visible tool specs.
    pub(crate) fn specs(&self) -> &'static [ToolSpec] {
        self.policy.specs()
    }

    /// Returns `true` when every named tool can execute in parallel.
    pub(crate) fn supports_parallel(&self, name: &str) -> bool {
        self.specs()
            .iter()
            .find(|spec| spec.name() == name)
            .is_some_and(|spec| spec.supports_parallel())
    }

    /// Executes a model tool call and converts failures into model-visible output.
    pub(crate) async fn execute(&self, call: ModelToolCall) -> ToolExecution {
        if !self.specs().iter().any(|spec| spec.name() == call.name) {
            return ToolExecution {
                call_id: call.call_id,
                tool_name: call.name,
                output: "Tool error: tool is not enabled by the current policy".to_string(),
                success: false,
            };
        }

        let result = match call.name.as_str() {
            READ_FILE_TOOL => read_file(&self.root, &call.arguments).await,
            LIST_DIR_TOOL => list_dir(&self.root, &call.arguments).await,
            SEARCH_FILES_TOOL => {
                let search = self.search.clone();
                let arguments = call.arguments.clone();
                tokio::task::spawn_blocking(move || search.search_files(&arguments))
                    .await
                    .context("search_files task failed")
                    .and_then(|result| result)
            }
            SEARCH_TEXT_TOOL => {
                let search = self.search.clone();
                let arguments = call.arguments.clone();
                tokio::task::spawn_blocking(move || search.search_text(&arguments))
                    .await
                    .context("search_text task failed")
                    .and_then(|result| result)
            }
            APPLY_PATCH_TOOL => apply_patch(&self.root, &call.arguments).await,
            EXEC_COMMAND_TOOL => exec_command(&self.root, &call.arguments).await,
            GIT_STATUS_TOOL => git_status(&self.root, &call.arguments).await,
            GIT_DIFF_TOOL => git_diff(&self.root, &call.arguments).await,
            GIT_LOG_TOOL => git_log(&self.root, &call.arguments).await,
            GIT_COMMIT_TOOL => git_commit(&self.root, &call.arguments).await,
            _ => Err(anyhow::anyhow!("unknown tool {}", call.name)),
        };

        match result {
            Ok(output) => ToolExecution {
                call_id: call.call_id,
                tool_name: call.name,
                output,
                success: true,
            },
            Err(error) => ToolExecution {
                call_id: call.call_id,
                tool_name: call.name,
                output: format!("Tool error: {error}"),
                success: false,
            },
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArguments {
    path: String,
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
    let args = parse_path_arguments(arguments)?;
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

async fn list_dir(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_path_arguments(arguments)?;
    let path = resolve_workspace_path(root, &args.path)?;
    let mut entries = tokio::fs::read_dir(&path)
        .await
        .with_context(|| format!("failed to list {}", display_path(root, &path)))?;
    let mut names = Vec::new();

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
        names.push(name);
    }

    names.sort_unstable();
    let truncated = names.len() > MAX_DIR_ENTRIES;
    names.truncate(MAX_DIR_ENTRIES);
    if truncated {
        names.push("[truncated: directory has more than 200 entries]".to_string());
    }
    Ok(names.join("\n"))
}

async fn exec_command(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_exec_command_arguments(arguments)?;
    let command = trimmed_required("command", &args.command)?;
    anyhow::ensure!(
        !mentions_git_executable(command),
        "exec_command cannot run git; use git_status, git_diff, git_log, or git_commit"
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
    )
    .await?;
    process_result_output(result)
}

async fn git_status(root: &Path, arguments: &str) -> Result<String> {
    let _args = parse_git_status_arguments(arguments)?;
    let args = vec![
        "status".to_string(),
        "--short".to_string(),
        "--branch".to_string(),
    ];
    let result = run_git(root, args, DEFAULT_COMMAND_OUTPUT_BYTES).await?;
    process_result_output(result)
}

async fn git_diff(root: &Path, arguments: &str) -> Result<String> {
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

    let result = run_git(root, git_args, max_output_bytes).await?;
    process_result_output(result)
}

async fn git_log(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_git_log_arguments(arguments)?;
    let max_count = bounded_limit(args.max_count, DEFAULT_GIT_LOG_COUNT, MAX_GIT_LOG_COUNT);
    let git_args = vec![
        "log".to_string(),
        "--oneline".to_string(),
        "--decorate".to_string(),
        "-n".to_string(),
        max_count.to_string(),
    ];

    let result = run_git(root, git_args, DEFAULT_COMMAND_OUTPUT_BYTES).await?;
    process_result_output(result)
}

async fn git_commit(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_git_commit_arguments(arguments)?;
    let message = trimmed_required("message", &args.message)?;
    anyhow::ensure!(!args.paths.is_empty(), "paths cannot be empty");
    let mut pathspecs = Vec::with_capacity(args.paths.len());
    for path in &args.paths {
        pathspecs.push(git_pathspec(path)?);
    }

    let mut add_args = vec!["add".to_string(), "--".to_string()];
    add_args.extend(pathspecs.iter().cloned());
    let add_result = run_git(root, add_args, DEFAULT_COMMAND_OUTPUT_BYTES).await?;
    let add_output = process_result_output(add_result)?;

    let mut commit_args = vec![
        "commit".to_string(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    commit_args.extend(pathspecs);
    let commit_result = run_git(root, commit_args, DEFAULT_COMMAND_OUTPUT_BYTES).await?;
    let commit_output = process_result_output(commit_result)?;

    Ok(format!(
        "git add:\n{add_output}\n\ngit commit:\n{commit_output}"
    ))
}

async fn apply_patch(root: &Path, arguments: &str) -> Result<String> {
    let args = parse_apply_patch_arguments(arguments)?;
    anyhow::ensure!(
        args.patch.len() <= MAX_PATCH_BYTES,
        "patch exceeds {MAX_PATCH_BYTES} bytes"
    );

    let patch = ParsedPatch::parse(&args.patch)?;
    let changes = plan_patch_changes(root, patch).await?;
    for change in &changes {
        match change {
            PlannedChange::Write {
                path,
                content,
                permissions,
                ..
            } => write_file_atomically(path, content, permissions.clone()).await?,
            PlannedChange::Delete { path, .. } => tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("failed to delete {}", display_path(root, path)))?,
        }
    }

    Ok(format_patch_summary(&changes))
}

async fn plan_patch_changes(root: &Path, patch: ParsedPatch) -> Result<Vec<PlannedChange>> {
    anyhow::ensure!(
        !patch.operations.is_empty(),
        "patch contains no file operations"
    );
    let mut changes = Vec::with_capacity(patch.operations.len());
    let mut seen = Vec::with_capacity(patch.operations.len());

    for operation in patch.operations {
        let change = match operation {
            PatchOperation::Add { path, content } => {
                let path = resolve_new_workspace_path(root, &path)?;
                ensure_unique_patch_path(&mut seen, &path)?;
                ensure_new_file_target(root, &path).await?;
                ensure_patch_file_size(content.len() as u64)?;
                PlannedChange::Write {
                    display_path: display_path(root, &path),
                    before_bytes: None,
                    after_bytes: content.len(),
                    path,
                    content,
                    permissions: None,
                }
            }
            PatchOperation::Update { path, hunks } => {
                let path = resolve_workspace_path(root, &path)?;
                ensure_unique_patch_path(&mut seen, &path)?;
                let (content, permissions) = read_patch_target(root, &path).await?;
                let before_bytes = content.len();
                let content = apply_hunks(&content, &hunks, &display_path(root, &path))?;
                ensure_patch_file_size(content.len() as u64)?;
                PlannedChange::Write {
                    display_path: display_path(root, &path),
                    before_bytes: Some(before_bytes),
                    after_bytes: content.len(),
                    path,
                    content,
                    permissions: Some(permissions),
                }
            }
            PatchOperation::Delete { path } => {
                let path = resolve_workspace_path(root, &path)?;
                ensure_unique_patch_path(&mut seen, &path)?;
                let (content, _permissions) = read_patch_target(root, &path).await?;
                PlannedChange::Delete {
                    display_path: display_path(root, &path),
                    before_bytes: content.len(),
                    path,
                }
            }
        };
        changes.push(change);
    }

    Ok(changes)
}

fn parse_path_arguments(arguments: &str) -> Result<PathArguments> {
    serde_json::from_str(arguments).context("invalid path arguments")
}

fn parse_search_files_arguments(arguments: &str) -> Result<SearchFilesArguments> {
    serde_json::from_str(arguments).context("invalid search_files arguments")
}

fn parse_search_text_arguments(arguments: &str) -> Result<SearchTextArguments> {
    serde_json::from_str(arguments).context("invalid search_text arguments")
}

fn parse_apply_patch_arguments(arguments: &str) -> Result<ApplyPatchArguments> {
    serde_json::from_str(arguments).context("invalid apply_patch arguments")
}

fn parse_exec_command_arguments(arguments: &str) -> Result<ExecCommandArguments> {
    serde_json::from_str(arguments).context("invalid exec_command arguments")
}

fn parse_git_status_arguments(arguments: &str) -> Result<GitStatusArguments> {
    serde_json::from_str(arguments).context("invalid git_status arguments")
}

fn parse_git_diff_arguments(arguments: &str) -> Result<GitDiffArguments> {
    serde_json::from_str(arguments).context("invalid git_diff arguments")
}

fn parse_git_log_arguments(arguments: &str) -> Result<GitLogArguments> {
    serde_json::from_str(arguments).context("invalid git_log arguments")
}

fn parse_git_commit_arguments(arguments: &str) -> Result<GitCommitArguments> {
    serde_json::from_str(arguments).context("invalid git_commit arguments")
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
    Ok(relative.to_string_lossy().into_owned())
}

async fn run_git(root: &Path, args: Vec<String>, max_output_bytes: usize) -> Result<ProcessResult> {
    run_process(
        root,
        "git",
        &args,
        root,
        DEFAULT_COMMAND_TIMEOUT_MS,
        max_output_bytes,
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
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {display:?}"))?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture command stderr")?;
    let stdout_task = tokio::spawn(read_limited_output(stdout, max_output_bytes));
    let stderr_task = tokio::spawn(read_limited_output(stderr, max_output_bytes));

    let mut timed_out = false;
    let status = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(status) => Some(status.context("failed to wait for command")?),
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            let _ = child.wait().await;
            None
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

    Ok(ProcessResult {
        display,
        cwd: display_path(root, cwd),
        timeout_ms,
        status,
        timed_out,
        stdout,
        stderr,
    })
}

async fn read_limited_output<R>(mut reader: R, max_bytes: usize) -> Result<LimitedOutput>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut output = LimitedOutput {
        bytes: Vec::with_capacity(max_bytes.min(8192)),
        total_bytes: 0,
    };
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            return Ok(output);
        }
        output.total_bytes = output.total_bytes.saturating_add(bytes_read);
        if output.bytes.len() < max_bytes {
            let remaining = max_bytes - output.bytes.len();
            output
                .bytes
                .extend_from_slice(&buffer[..bytes_read.min(remaining)]);
        }
    }
}

#[derive(Debug)]
struct ProcessResult {
    display: String,
    cwd: String,
    timeout_ms: u64,
    status: Option<std::process::ExitStatus>,
    timed_out: bool,
    stdout: LimitedOutput,
    stderr: LimitedOutput,
}

#[derive(Debug)]
struct LimitedOutput {
    bytes: Vec<u8>,
    total_bytes: usize,
}

impl LimitedOutput {
    fn is_truncated(&self) -> bool {
        self.total_bytes > self.bytes.len()
    }
}

fn process_result_output(result: ProcessResult) -> Result<String> {
    let success = result
        .status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success)
        && !result.timed_out;
    let output = format_process_result(&result);
    if success {
        Ok(output)
    } else {
        anyhow::bail!("{output}")
    }
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
    output.push_str(&String::from_utf8_lossy(&limited.bytes));
    if limited.is_truncated() {
        output.push_str(&format!(
            "\n[truncated: {name} captured {} of {} bytes]",
            limited.bytes.len(),
            limited.total_bytes
        ));
    }
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

fn mentions_git_executable(command: &str) -> bool {
    let mut token = String::new();
    for ch in command.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\') {
            token.push(ch);
        } else {
            if is_git_executable_token(&token) {
                return true;
            }
            token.clear();
        }
    }
    is_git_executable_token(&token)
}

fn is_git_executable_token(token: &str) -> bool {
    let name = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    name == "git" || name == "git.exe"
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

    let mut output = String::new();
    if let Some(error) = &results.regex_fallback_error {
        output.push_str("Regex fallback: ");
        output.push_str(error);
        output.push('\n');
    }
    for search_match in &results.matches {
        let Some(file) = results.files.get(search_match.file_index) else {
            continue;
        };
        output.push_str(&format!(
            "{}:{}:{}: {}\n",
            file.relative_path(picker),
            search_match.line_number,
            search_match.col.saturating_add(1),
            search_match.line_content
        ));
        for line in &search_match.context_before {
            output.push_str("  before: ");
            output.push_str(line);
            output.push('\n');
        }
        for line in &search_match.context_after {
            output.push_str("  after: ");
            output.push_str(line);
            output.push('\n');
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
    output
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
enum PlannedChange {
    Write {
        display_path: String,
        before_bytes: Option<usize>,
        after_bytes: usize,
        path: PathBuf,
        content: String,
        permissions: Option<std::fs::Permissions>,
    },
    Delete {
        display_path: String,
        before_bytes: usize,
        path: PathBuf,
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

fn ensure_unique_patch_path(seen: &mut Vec<PathBuf>, path: &Path) -> Result<()> {
    anyhow::ensure!(
        !seen.iter().any(|seen_path| seen_path == path),
        "patch touches {} more than once",
        path.display()
    );
    seen.push(path.to_path_buf());
    Ok(())
}

async fn write_file_atomically(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<()> {
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

    if let Err(error) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
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

fn format_patch_summary(changes: &[PlannedChange]) -> String {
    let mut output = format!("Applied patch: {} file(s) changed", changes.len());
    for change in changes {
        output.push('\n');
        match change {
            PlannedChange::Write {
                display_path,
                before_bytes: Some(before_bytes),
                after_bytes,
                ..
            } => {
                output.push_str(&format!(
                    "updated {display_path} ({before_bytes} -> {after_bytes} bytes)"
                ));
            }
            PlannedChange::Write {
                display_path,
                before_bytes: None,
                after_bytes,
                ..
            } => {
                output.push_str(&format!("added {display_path} ({after_bytes} bytes)"));
            }
            PlannedChange::Delete {
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
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::bench_support::DurationSummary;

    #[test]
    fn serializes_tool_specs_without_allocating_json_values() {
        assert_eq!(
            serde_json::to_value(READ_ONLY_TOOL_SPECS).unwrap(),
            json!([
                {
                    "type": "function",
                    "name": "read_file",
                    "description": "Read a UTF-8 text file from the current workspace.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Workspace-relative path to the file to read.",
                            },
                        },
                        "required": ["path"],
                        "additionalProperties": false,
                    },
                },
                {
                    "type": "function",
                    "name": "list_dir",
                    "description": "List files and directories under a workspace directory.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Workspace-relative directory path. Use . for the workspace root.",
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
    fn workspace_write_policy_exposes_apply_patch() {
        let specs = ToolPolicy::WorkspaceWrite.specs();

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
        let specs = ToolPolicy::WorkspaceExec.specs();

        assert!(specs.iter().any(|spec| spec.name() == APPLY_PATCH_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == EXEC_COMMAND_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_STATUS_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_DIFF_TOOL));
        assert!(specs.iter().any(|spec| spec.name() == GIT_LOG_TOOL));
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
                "description": "Execute a non-git shell command from the workspace root and return bounded output.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute. Direct git commands are rejected; use git_* tools.",
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
        assert!(list.success);
        assert_eq!(list.output, "src/");

        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn executes_shell_commands_and_blocks_git() {
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
        let blocked = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_git_block".to_string(),
                name: EXEC_COMMAND_TOOL.to_string(),
                arguments: json!({ "command": "git status --short" }).to_string(),
            })
            .await;

        assert!(command.success, "{}", command.output);
        assert!(command.output.contains("src"), "{}", command.output);
        assert!(!blocked.success);
        assert!(
            blocked.output.contains("cannot run git"),
            "{}",
            blocked.output
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

        assert!(status.success, "{}", status.output);
        assert!(status.output.contains(" M README.md"), "{}", status.output);
        assert!(diff.success, "{}", diff.output);
        assert!(diff.output.contains("-old"), "{}", diff.output);
        assert!(diff.output.contains("+new"), "{}", diff.output);
        assert!(commit.success, "{}", commit.output);
        assert!(commit.output.contains("git commit"), "{}", commit.output);
        assert!(log.success, "{}", log.output);
        assert!(log.output.contains("Update readme"), "{}", log.output);

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
