//! Built-in tool registry and execution.

use std::fs as sync_fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use globset::Glob;
use globset::GlobSet;
use globset::GlobSetBuilder;
use ignore::WalkBuilder;
use regex::Regex;
use serde::ser::SerializeMap;
use serde::ser::SerializeStruct;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::agent_loop::ModelToolCall;

const READ_FILE_TOOL: &str = "read_file";
const LIST_DIR_TOOL: &str = "list_dir";
const SEARCH_FILES_TOOL: &str = "search_files";
const SEARCH_TEXT_TOOL: &str = "search_text";
const APPLY_PATCH_TOOL: &str = "apply_patch";
const MAX_FILE_BYTES: usize = 64 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n[truncated: file exceeds 65536 bytes]";
const MAX_DIR_ENTRIES: usize = 200;
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_PATCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_OFFSET: usize = 100_000;
const MAX_TEXT_CONTEXT_LINES: usize = 3;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;

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
        description: "Fuzzy-search workspace files and directories by path.",
        parameters: ToolParameters::SearchFiles,
        supports_parallel: false,
    },
    ToolSpec {
        name: SEARCH_TEXT_TOOL,
        description: "Search workspace file contents with literal, regex, or fuzzy matching.",
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
        description: "Fuzzy-search workspace files and directories by path.",
        parameters: ToolParameters::SearchFiles,
        supports_parallel: false,
    },
    ToolSpec {
        name: SEARCH_TEXT_TOOL,
        description: "Search workspace file contents with literal, regex, or fuzzy matching.",
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

/// Permission set controlling which built-in tools are exposed and executable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToolPolicy {
    #[default]
    ReadOnly,
    WorkspaceWrite,
}

impl ToolPolicy {
    const fn specs(self) -> &'static [ToolSpec] {
        match self {
            Self::ReadOnly => READ_ONLY_TOOL_SPECS,
            Self::WorkspaceWrite => WORKSPACE_WRITE_TOOL_SPECS,
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
                description: "Text query. Glob path constraints like src/*.rs may be included.",
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

/// Registry for built-in tools.
#[derive(Clone, Debug)]
pub(crate) struct ToolRegistry {
    policy: ToolPolicy,
    root: Arc<PathBuf>,
    search: WorkspaceSearchIndex,
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
            search: WorkspaceSearchIndex::new(root.clone()),
            root: Arc::new(root),
        }
    }

    /// Initializes the workspace search snapshot on a blocking worker.
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

#[derive(Clone, Debug)]
struct WorkspaceSearchIndex {
    root: Arc<PathBuf>,
    state: Arc<Mutex<Option<WorkspaceSearchState>>>,
}

impl WorkspaceSearchIndex {
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
        let mut matches = state
            .entries
            .iter()
            .filter_map(|entry| {
                Some(SearchFileMatch {
                    relative: entry.relative.clone(),
                    is_dir: entry.is_dir,
                    score: score_path(query, entry)?,
                })
            })
            .collect::<Vec<_>>();
        let total_matched = matches.len();
        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.relative.cmp(&right.relative))
        });
        let items = matches
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();

        Ok(format_file_search_results(
            &items,
            &state,
            total_matched,
            offset,
            limit,
        ))
    }

    fn search_text(&self, arguments: &str) -> Result<String> {
        let args = parse_search_text_arguments(arguments)?;
        let query = trimmed_required("query", &args.query)?;
        let limit = bounded_limit(args.limit, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS);
        let file_offset = args.file_offset.unwrap_or(0).min(MAX_SEARCH_OFFSET);
        let before_context = args.before_context.unwrap_or(0).min(MAX_TEXT_CONTEXT_LINES);
        let after_context = args.after_context.unwrap_or(0).min(MAX_TEXT_CONTEXT_LINES);
        let state = self.ready_state()?;
        let query = TextSearchQuery::parse(query)?;
        let matcher = TextMatcher::new(parse_search_text_mode(args.mode.as_deref())?, &query.text)?;
        let results = search_text_entries(
            &state,
            query.path_filter.as_ref(),
            &matcher,
            TextSearchOptions {
                limit,
                file_offset,
                before_context,
                after_context,
            },
        );

        Ok(format_text_search_results(&results))
    }

    fn ready_state(&self) -> Result<WorkspaceSearchState> {
        self.state()
    }

    fn state(&self) -> Result<WorkspaceSearchState> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("search index lock was poisoned"))?;
        if let Some(state) = state.as_ref() {
            return Ok(state.clone());
        }

        let initialized = WorkspaceSearchState::new(&self.root)?;
        *state = Some(initialized.clone());
        Ok(initialized)
    }
}

#[derive(Clone, Debug)]
struct WorkspaceSearchState {
    entries: Arc<Vec<SearchEntry>>,
    total_files: usize,
    total_dirs: usize,
}

impl WorkspaceSearchState {
    fn new(root: &Path) -> Result<Self> {
        let mut entries = Vec::new();
        let mut total_files = 0usize;
        let mut total_dirs = 0usize;

        for entry in WalkBuilder::new(root)
            .standard_filters(true)
            .follow_links(false)
            .build()
            .filter_map(std::result::Result::ok)
        {
            let path = entry.path();
            if path == root {
                continue;
            }
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() && !file_type.is_dir() {
                continue;
            }

            let relative = normalized_relative_path(root, path)?;
            let is_dir = file_type.is_dir();
            let size = if is_dir {
                total_dirs += 1;
                0
            } else {
                total_files += 1;
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            };
            entries.push(SearchEntry {
                relative,
                full_path: path.to_path_buf(),
                is_dir,
                size,
            });
        }

        entries.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(Self {
            entries: Arc::new(entries),
            total_files,
            total_dirs,
        })
    }
}

#[derive(Clone, Debug)]
struct SearchEntry {
    relative: String,
    full_path: PathBuf,
    is_dir: bool,
    size: u64,
}

#[derive(Debug)]
struct SearchFileMatch {
    relative: String,
    is_dir: bool,
    score: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchTextMode {
    Plain,
    Regex,
    Fuzzy,
}

struct TextSearchQuery {
    text: String,
    path_filter: Option<GlobSet>,
}

impl TextSearchQuery {
    fn parse(query: &str) -> Result<Self> {
        let mut text_parts = Vec::new();
        let mut path_globs = Vec::new();
        let parts = query.split_whitespace().collect::<Vec<_>>();

        if parts.len() > 1 {
            for part in parts {
                if looks_like_path_glob(part) {
                    path_globs.push(part);
                } else {
                    text_parts.push(part);
                }
            }
        }

        if text_parts.is_empty() {
            return Ok(Self {
                text: query.to_string(),
                path_filter: None,
            });
        }

        let has_path_globs = !path_globs.is_empty();
        let mut builder = GlobSetBuilder::new();
        for glob in path_globs {
            builder.add(Glob::new(glob).with_context(|| format!("invalid path glob {glob:?}"))?);
        }
        let path_filter = if has_path_globs {
            Some(
                builder
                    .build()
                    .context("failed to build path glob filter")?,
            )
        } else {
            None
        };

        Ok(Self {
            text: text_parts.join(" "),
            path_filter,
        })
    }
}

enum TextMatcher {
    Plain(String),
    Regex(Regex),
    Fuzzy(String),
}

impl TextMatcher {
    fn new(mode: SearchTextMode, query: &str) -> Result<Self> {
        match mode {
            SearchTextMode::Plain => Ok(Self::Plain(query.to_ascii_lowercase())),
            SearchTextMode::Regex => Regex::new(query)
                .map(Self::Regex)
                .with_context(|| format!("invalid regex search query {query:?}")),
            SearchTextMode::Fuzzy => Ok(Self::Fuzzy(query.to_ascii_lowercase())),
        }
    }

    fn find_in_line(&self, line: &str) -> Option<usize> {
        match self {
            Self::Plain(query) => line.to_ascii_lowercase().find(query),
            Self::Regex(regex) => regex.find(line).map(|matched| matched.start()),
            Self::Fuzzy(query) => fuzzy_match_start(query, &line.to_ascii_lowercase()),
        }
    }
}

#[derive(Clone, Copy)]
struct TextSearchOptions {
    limit: usize,
    file_offset: usize,
    before_context: usize,
    after_context: usize,
}

#[derive(Debug)]
struct TextSearchResults {
    matches: Vec<TextSearchMatch>,
    files_with_matches: usize,
    searched_files: usize,
    searchable_files: usize,
    total_files: usize,
    next_file_offset: usize,
}

#[derive(Debug)]
struct TextSearchMatch {
    relative: String,
    line_number: usize,
    col: usize,
    line: String,
    context_before: Vec<String>,
    context_after: Vec<String>,
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

fn trimmed_required<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{name} cannot be empty");
    Ok(value)
}

fn bounded_limit(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.unwrap_or(default).clamp(1, max)
}

fn parse_search_text_mode(mode: Option<&str>) -> Result<SearchTextMode> {
    let mode = mode.map(str::trim).filter(|mode| !mode.is_empty());
    let mode = mode.map(str::to_ascii_lowercase);
    match mode.as_deref() {
        None | Some("plain") | Some("literal") => Ok(SearchTextMode::Plain),
        Some("regex") => Ok(SearchTextMode::Regex),
        Some("fuzzy") => Ok(SearchTextMode::Fuzzy),
        Some(mode) => anyhow::bail!("unsupported search_text mode {mode:?}"),
    }
}

fn format_file_search_results(
    items: &[SearchFileMatch],
    state: &WorkspaceSearchState,
    total_matched: usize,
    offset: usize,
    limit: usize,
) -> String {
    if items.is_empty() {
        return format!(
            "No files or directories matched. total_files={} total_dirs={}",
            state.total_files, state.total_dirs
        );
    }

    let mut output = String::new();
    for item in items {
        output.push_str(&item.relative);
        if item.is_dir && !item.relative.ends_with('/') {
            output.push('/');
        }
        output.push('\n');
    }
    output.push_str(&format!(
        "[matched={} total_files={} total_dirs={} offset={} limit={}]",
        total_matched, state.total_files, state.total_dirs, offset, limit
    ));
    output
}

fn search_text_entries(
    state: &WorkspaceSearchState,
    path_filter: Option<&GlobSet>,
    matcher: &TextMatcher,
    options: TextSearchOptions,
) -> TextSearchResults {
    let candidates = state
        .entries
        .iter()
        .filter(|entry| {
            !entry.is_dir
                && path_filter.map_or(true, |filter| filter.is_match(Path::new(&entry.relative)))
        })
        .collect::<Vec<_>>();
    let mut results = TextSearchResults {
        matches: Vec::new(),
        files_with_matches: 0,
        searched_files: 0,
        searchable_files: candidates.len(),
        total_files: state.total_files,
        next_file_offset: 0,
    };

    for (candidate_index, entry) in candidates.iter().enumerate().skip(options.file_offset) {
        if results.matches.len() >= options.limit {
            results.next_file_offset = candidate_index;
            break;
        }
        results.searched_files += 1;
        if entry.size > MAX_SEARCH_FILE_BYTES {
            continue;
        }

        let Ok(text) = sync_fs::read_to_string(&entry.full_path) else {
            continue;
        };
        let lines = text.lines().collect::<Vec<_>>();
        let mut file_had_match = false;

        for (line_index, line) in lines.iter().enumerate() {
            let Some(col) = matcher.find_in_line(line) else {
                continue;
            };

            file_had_match = true;
            let before_start = line_index.saturating_sub(options.before_context);
            let after_end = (line_index + options.after_context + 1).min(lines.len());
            results.matches.push(TextSearchMatch {
                relative: entry.relative.clone(),
                line_number: line_index + 1,
                col: col + 1,
                line: (*line).to_string(),
                context_before: lines[before_start..line_index]
                    .iter()
                    .map(|line| (*line).to_string())
                    .collect(),
                context_after: lines[line_index + 1..after_end]
                    .iter()
                    .map(|line| (*line).to_string())
                    .collect(),
            });

            if results.matches.len() >= options.limit {
                break;
            }
        }

        if file_had_match {
            results.files_with_matches += 1;
        }
        if results.matches.len() >= options.limit {
            results.next_file_offset = candidate_index.saturating_add(1);
            break;
        }
    }

    results
}

fn format_text_search_results(results: &TextSearchResults) -> String {
    if results.matches.is_empty() {
        return format!(
            "No text matched. searched_files={} searchable_files={} total_files={}",
            results.searched_files, results.searchable_files, results.total_files
        );
    }

    let mut output = String::new();
    for search_match in &results.matches {
        output.push_str(&format!(
            "{}:{}:{}: {}\n",
            search_match.relative, search_match.line_number, search_match.col, search_match.line
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
        results.searched_files,
        results.searchable_files,
        results.next_file_offset,
    ));
    output
}

fn score_path(query: &str, entry: &SearchEntry) -> Option<i64> {
    let candidate = entry.relative.to_ascii_lowercase();
    let mut score = if entry.is_dir { 0 } else { 25 };
    for term in query.split_whitespace() {
        let term = term.to_ascii_lowercase();
        if let Some(index) = candidate.find(&term) {
            score += 10_000 - index.min(10_000) as i64 + term.len() as i64 * 100;
        } else if let Some(index) = fuzzy_match_start(&term, &candidate) {
            score += 1_000 - index.min(1_000) as i64;
        } else {
            return None;
        }
    }
    Some(score)
}

fn fuzzy_match_start(query: &str, candidate: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }

    let mut first = None;
    let mut chars = candidate.char_indices();
    for needle in query.chars() {
        let mut found = None;
        for (index, candidate) in chars.by_ref() {
            if candidate == needle {
                found = Some(index);
                break;
            }
        }
        let found = found?;
        first.get_or_insert(found);
    }
    first
}

fn looks_like_path_glob(part: &str) -> bool {
    part.contains('/') && (part.contains('*') || part.contains('?') || part.contains('['))
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("path escaped workspace: {}", path.display()))?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    anyhow::ensure!(!parts.is_empty(), "search path cannot be empty");
    Ok(parts.join("/"))
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
                    "description": "Fuzzy-search workspace files and directories by path.",
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
                    "description": "Search workspace file contents with literal, regex, or fuzzy matching.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Text query. Glob path constraints like src/*.rs may be included.",
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
    async fn executes_workspace_search_tools() {
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
        let regex_text = registry
            .execute(ModelToolCall {
                item_id: None,
                call_id: "call_search_text_regex".to_string(),
                name: SEARCH_TEXT_TOOL.to_string(),
                arguments: r#"{"query":"src/*.rs special_[a-z]+","mode":"regex","limit":5}"#
                    .to_string(),
            })
            .await;

        assert!(files.success, "{}", files.output);
        assert!(files.output.contains("src/main.rs"), "{}", files.output);
        assert!(text.success, "{}", text.output);
        assert!(text.output.contains("src/main.rs:2:"), "{}", text.output);
        assert!(text.output.contains("special_needle"), "{}", text.output);
        assert!(regex_text.success, "{}", regex_text.output);
        assert!(
            regex_text.output.contains("src/main.rs:2:"),
            "{}",
            regex_text.output
        );

        drop(registry);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[tokio::test]
    async fn initializes_workspace_search_snapshot_before_first_search() {
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
    #[ignore = "release-mode workspace search benchmark; run explicitly with --ignored --nocapture"]
    async fn benchmark_workspace_search_current_repo() {
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
            "workspace_search_current_repo samples={SAMPLES} cold_file_search_ms={:.3} warm_file_min_ms={:.3} warm_file_median_ms={:.3} warm_file_max_ms={:.3} warm_file_output_bytes={file_output_bytes} warm_text_min_ms={:.3} warm_text_median_ms={:.3} warm_text_max_ms={:.3} warm_text_output_bytes={text_output_bytes}",
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
}
