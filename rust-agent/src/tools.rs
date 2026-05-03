//! Built-in tool registry and execution.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use serde::ser::SerializeMap;
use serde::ser::SerializeStruct;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;

use crate::agent_loop::ModelToolCall;

const READ_FILE_TOOL: &str = "read_file";
const LIST_DIR_TOOL: &str = "list_dir";
const MAX_FILE_BYTES: usize = 64 * 1024;
const FILE_TRUNCATION_MARKER: &str = "\n[truncated: file exceeds 65536 bytes]";
const MAX_DIR_ENTRIES: usize = 200;

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
}

impl ToolParameters {
    const fn cache_key(self) -> &'static str {
        match self {
            Self::Path { .. } => "path:string:required:no_additional_properties",
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

/// Registry for built-in tools.
#[derive(Clone, Debug)]
pub(crate) struct ToolRegistry {
    root: Arc<PathBuf>,
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

    use serde_json::json;

    use super::*;

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
        assert!(read.output[..MAX_FILE_BYTES]
            .as_bytes()
            .iter()
            .all(|byte| *byte == b'a'));
        assert!(read.output.ends_with(FILE_TRUNCATION_MARKER));

        fs::remove_dir_all(&temp).unwrap();
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
}
