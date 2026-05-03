//! Built-in tool registry and execution.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use fff_search::file_picker::FilePicker;
use fff_search::grep::GrepMode;
use fff_search::grep::GrepSearchOptions;
use fff_search::grep::parse_grep_query;
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

use crate::agent_loop::ModelToolCall;

const READ_FILE_TOOL: &str = "read_file";
const LIST_DIR_TOOL: &str = "list_dir";
const SEARCH_FILES_TOOL: &str = "search_files";
const SEARCH_TEXT_TOOL: &str = "search_text";
const MAX_FILE_BYTES: usize = 64 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n[truncated: file exceeds 65536 bytes]";
const MAX_DIR_ENTRIES: usize = 200;
const DEFAULT_SEARCH_RESULTS: usize = 20;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_OFFSET: usize = 100_000;
const DEFAULT_TEXT_SEARCH_TIMEOUT_MS: u64 = 250;
const MAX_TEXT_CONTEXT_LINES: usize = 3;
const FFF_SCAN_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

const TOOL_SPECS: &[ToolSpec] = &[
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
        description: "Search indexed workspace file contents with literal, regex, or fuzzy matching.",
        parameters: ToolParameters::SearchText,
        supports_parallel: false,
    },
];

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
}

impl ToolParameters {
    const fn cache_key(self) -> &'static str {
        match self {
            Self::Path { .. } => "path:string:required:no_additional_properties",
            Self::SearchFiles => "search_files:query:string:required:limit:integer:offset:integer",
            Self::SearchText => {
                "search_text:query:string:required:mode:string:limit:integer:file_offset:integer:context:integer"
            }
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

/// Registry for built-in tools.
#[derive(Clone, Debug)]
pub(crate) struct ToolRegistry {
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
    /// Creates a tool registry rooted at `root`.
    pub(crate) fn for_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self {
            search: FffSearchIndex::new(root.clone()),
            root: Arc::new(root),
        }
    }

    /// Returns the stable model-visible tool specs.
    pub(crate) const fn specs(&self) -> &'static [ToolSpec] {
        TOOL_SPECS
    }

    /// Returns `true` when every named tool can execute in parallel.
    pub(crate) fn supports_parallel(&self, name: &str) -> bool {
        TOOL_SPECS
            .iter()
            .find(|spec| spec.name() == name)
            .is_some_and(|spec| spec.supports_parallel())
    }

    /// Executes a model tool call and converts failures into model-visible output.
    pub(crate) async fn execute(&self, call: ModelToolCall) -> ToolExecution {
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
        anyhow::ensure!(
            state.picker.wait_for_scan(FFF_SCAN_WAIT_TIMEOUT),
            "search index is still scanning; retry shortly"
        );
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

fn parse_path_arguments(arguments: &str) -> Result<PathArguments> {
    serde_json::from_str(arguments).context("invalid path arguments")
}

fn parse_search_files_arguments(arguments: &str) -> Result<SearchFilesArguments> {
    serde_json::from_str(arguments).context("invalid search_files arguments")
}

fn parse_search_text_arguments(arguments: &str) -> Result<SearchTextArguments> {
    serde_json::from_str(arguments).context("invalid search_text arguments")
}

fn trimmed_required<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{name} cannot be empty");
    Ok(value)
}

fn bounded_limit(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.unwrap_or(default).clamp(1, max)
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
            serde_json::to_value(TOOL_SPECS).unwrap(),
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
