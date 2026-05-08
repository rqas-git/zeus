//! SQLite-backed session storage.

use std::ffi::CStr;
use std::ffi::CString;
use std::os::raw::c_char;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use libsqlite3_sys as ffi;

use crate::agent_loop::AgentItem;
use crate::agent_loop::AgentMessage;
use crate::agent_loop::CacheObservation;
use crate::agent_loop::MessageId;
use crate::agent_loop::MessageRole;
use crate::agent_loop::SessionConfig;
use crate::agent_loop::SessionId;
use crate::agent_loop::SessionStatus;
use crate::compaction::CompactionDetails;

// SQLite busy waits cover short concurrent writes from the UI and background
// server tasks without hiding long-lived database contention.
const BUSY_TIMEOUT_MS: i32 = 5_000;
// Negative `PRAGMA cache_size` values are KiB. 64 MiB keeps large transcript
// scans fast without making each rust-agent process memory-heavy.
const SQLITE_CACHE_SIZE_KIB: i32 = 64_000;
// Session lists show a compact preview only; longer text belongs in the detail
// endpoint and would make list responses noisy.
const MAX_SESSION_PREVIEW_CHARS: i64 = 240;

/// Durable state for one loaded session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredSession {
    pub(crate) config: SessionConfig,
    pub(crate) status: SessionStatus,
    pub(crate) messages: Vec<AgentMessage>,
    pub(crate) last_cache_observation: Option<CacheObservation>,
}

/// Durable metadata for one stored session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredSessionMetadata {
    pub(crate) session_id: SessionId,
    pub(crate) model: String,
    pub(crate) status: SessionStatus,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) message_count: u64,
    pub(crate) last_message: Option<StoredSessionLastMessage>,
}

/// Preview metadata for the latest user or assistant message in a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredSessionLastMessage {
    pub(crate) message_id: MessageId,
    pub(crate) role: MessageRole,
    pub(crate) preview: String,
    pub(crate) truncated: bool,
    pub(crate) created_at_ms: i64,
}

/// SQLite-first storage for sessions and messages.
#[derive(Clone, Debug)]
pub(crate) struct SessionDatabase {
    inner: Arc<Mutex<Connection>>,
}

impl SessionDatabase {
    /// Opens a session database at `path`, creating the schema when needed.
    ///
    /// # Errors
    /// Returns an error when SQLite cannot open or initialize the database.
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            ensure_database_dir(parent)?;
        }
        Self::open_location(path)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self> {
        Self::open_location(Path::new(":memory:"))
    }

    fn open_location(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)?;
        connection.configure()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
        })
    }

    /// Inserts a session row if one does not already exist.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn ensure_session(&self, session_id: SessionId, model: &str) -> Result<()> {
        let now = now_millis()?;
        self.with_connection(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO sessions \
                 (id, model, status, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_text(2, model)?;
                    statement.bind_text(3, SessionStatus::Idle.as_str())?;
                    statement.bind_i64(4, now)?;
                    statement.bind_i64(5, now)
                },
            )
        })
    }

    /// Loads a session and its ordered messages, if it exists.
    ///
    /// # Errors
    /// Returns an error when stored rows cannot be read or decoded.
    pub(crate) fn load_session(&self, session_id: SessionId) -> Result<Option<StoredSession>> {
        self.with_connection(|connection| {
            let Some((config, status, last_cache_observation)) =
                load_session_header(connection, session_id)?
            else {
                return Ok(None);
            };
            let messages = load_session_messages(connection, session_id)?;
            Ok(Some(StoredSession {
                config,
                status,
                messages,
                last_cache_observation,
            }))
        })
    }

    /// Lists stored session metadata ordered by recent activity.
    ///
    /// # Errors
    /// Returns an error when stored rows cannot be read or decoded.
    pub(crate) fn list_session_metadata(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<StoredSessionMetadata>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT s.id,
                        s.model,
                        s.status,
                        s.created_at_ms,
                        s.updated_at_ms,
                        (SELECT COUNT(*)
                           FROM messages m_count
                          WHERE m_count.session_id = s.id) AS message_count,
                        lm.message_id,
                        lm.role,
                        substr(COALESCE(lm.text, ''), 1, ?3) AS last_preview,
                        length(COALESCE(lm.text, '')) > ?3 AS last_truncated,
                        lm.created_at_ms
                   FROM sessions s
                   LEFT JOIN messages lm
                     ON lm.session_id = s.id
                    AND lm.kind = 'message'
                    AND lm.message_id = (
                        SELECT m_last.message_id
                          FROM messages m_last
                         WHERE m_last.session_id = s.id
                           AND m_last.kind = 'message'
                         ORDER BY m_last.message_id DESC
                         LIMIT 1
                    )
                  ORDER BY s.updated_at_ms DESC, s.id DESC
                  LIMIT ?1 OFFSET ?2",
            )?;
            statement.bind_i64(1, usize_to_i64(limit, "session metadata limit")?)?;
            statement.bind_i64(2, usize_to_i64(offset, "session metadata offset")?)?;
            statement.bind_i64(3, MAX_SESSION_PREVIEW_CHARS)?;
            load_session_metadata_rows(&mut statement)
        })
    }

    /// Loads metadata for one stored session.
    ///
    /// # Errors
    /// Returns an error when the stored row cannot be read or decoded.
    pub(crate) fn session_metadata(
        &self,
        session_id: SessionId,
    ) -> Result<Option<StoredSessionMetadata>> {
        self.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT s.id,
                        s.model,
                        s.status,
                        s.created_at_ms,
                        s.updated_at_ms,
                        (SELECT COUNT(*)
                           FROM messages m_count
                          WHERE m_count.session_id = s.id) AS message_count,
                        lm.message_id,
                        lm.role,
                        substr(COALESCE(lm.text, ''), 1, ?2) AS last_preview,
                        length(COALESCE(lm.text, '')) > ?2 AS last_truncated,
                        lm.created_at_ms
                   FROM sessions s
                   LEFT JOIN messages lm
                     ON lm.session_id = s.id
                    AND lm.kind = 'message'
                    AND lm.message_id = (
                        SELECT m_last.message_id
                          FROM messages m_last
                         WHERE m_last.session_id = s.id
                           AND m_last.kind = 'message'
                         ORDER BY m_last.message_id DESC
                         LIMIT 1
                    )
                  WHERE s.id = ?1",
            )?;
            statement.bind_i64(1, session_id_i64(session_id)?)?;
            statement.bind_i64(2, MAX_SESSION_PREVIEW_CHARS)?;
            match statement.step()? {
                StepResult::Done => Ok(None),
                StepResult::Row => decode_session_metadata_row(&statement).map(Some),
            }
        })
    }

    /// Persists the current model for an existing session.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn set_session_model(&self, session_id: SessionId, model: &str) -> Result<()> {
        let now = now_millis()?;
        self.with_connection(|connection| {
            connection.execute(
                "UPDATE sessions SET model = ?2, updated_at_ms = ?3 WHERE id = ?1",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_text(2, model)?;
                    statement.bind_i64(3, now)
                },
            )
        })
    }

    /// Persists the current status for an existing session.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn set_session_status(
        &self,
        session_id: SessionId,
        status: SessionStatus,
    ) -> Result<()> {
        let now = now_millis()?;
        self.with_connection(|connection| {
            connection.execute(
                "UPDATE sessions SET status = ?2, updated_at_ms = ?3 WHERE id = ?1",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_text(2, status.as_str())?;
                    statement.bind_i64(3, now)
                },
            )
        })
    }

    /// Persists cache-health continuity for an existing session.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn record_cache_observation(
        &self,
        session_id: SessionId,
        observation: &CacheObservation,
    ) -> Result<()> {
        let now = now_millis()?;
        self.with_connection(|connection| {
            connection.execute(
                "UPDATE sessions \
                 SET last_prompt_cache_key = ?2, last_stable_prefix_hash = ?3, \
                     last_request_input_hash = ?4, last_request_input_message_count = ?5, \
                     updated_at_ms = ?6 \
                 WHERE id = ?1",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_text(2, &observation.prompt_cache_key)?;
                    statement.bind_i64(3, cache_hash_i64(observation.stable_prefix_hash))?;
                    statement
                        .bind_optional_i64(4, observation.request_input_hash.map(cache_hash_i64))?;
                    statement.bind_optional_i64(
                        5,
                        observation
                            .request_input_message_count
                            .map(|count| usize_to_i64(count, "request input message count"))
                            .transpose()?,
                    )?;
                    statement.bind_i64(6, now)
                },
            )
        })
    }

    /// Inserts one ordered message row.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn insert_message(
        &self,
        session_id: SessionId,
        message: &AgentMessage,
    ) -> Result<()> {
        let now = now_millis()?;
        self.with_connection(|connection| insert_message(connection, session_id, message, now))
    }

    /// Deletes one message row.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn delete_message(
        &self,
        session_id: SessionId,
        message_id: MessageId,
    ) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute(
                "DELETE FROM messages WHERE session_id = ?1 AND message_id = ?2",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_i64(2, message_id_i64(message_id)?)
                },
            )
        })
    }

    /// Deletes a session and all of its messages.
    ///
    /// # Errors
    /// Returns an error when the database write fails.
    pub(crate) fn delete_session(&self, session_id: SessionId) -> Result<bool> {
        self.with_connection(|connection| {
            connection.execute("DELETE FROM sessions WHERE id = ?1", |statement| {
                statement.bind_i64(1, session_id_i64(session_id)?)
            })?;
            Ok(connection.changes() > 0)
        })
    }

    fn with_connection<T>(&self, action: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("session database lock was poisoned"))?;
        action(&mut connection)
    }
}

fn load_session_metadata_rows(statement: &mut Statement<'_>) -> Result<Vec<StoredSessionMetadata>> {
    let mut sessions = Vec::new();
    while statement.step()? == StepResult::Row {
        sessions.push(decode_session_metadata_row(statement)?);
    }
    Ok(sessions)
}

fn decode_session_metadata_row(statement: &Statement<'_>) -> Result<StoredSessionMetadata> {
    let session_id = SessionId::new(i64_to_u64(statement.column_i64(0), "session id")?);
    let model = statement.column_text(1)?;
    let status = parse_session_status(&statement.column_text(2)?)?;
    let created_at_ms = statement.column_i64(3);
    let updated_at_ms = statement.column_i64(4);
    let message_count = i64_to_u64(statement.column_i64(5), "message count")?;
    let last_message = decode_session_last_message(statement)?;

    Ok(StoredSessionMetadata {
        session_id,
        model,
        status,
        created_at_ms,
        updated_at_ms,
        message_count,
        last_message,
    })
}

fn decode_session_last_message(
    statement: &Statement<'_>,
) -> Result<Option<StoredSessionLastMessage>> {
    let Some(message_id) = statement.column_optional_i64(6)? else {
        return Ok(None);
    };
    let role = statement
        .column_optional_text(7)?
        .as_deref()
        .and_then(MessageRole::from_str)
        .context("stored last message row has invalid role")?;
    let preview = statement.column_optional_text(8)?.unwrap_or_default();
    let truncated = statement.column_optional_i64(9)?.unwrap_or(0) != 0;
    let created_at_ms = statement
        .column_optional_i64(10)?
        .context("stored last message row is missing created_at_ms")?;

    Ok(Some(StoredSessionLastMessage {
        message_id: MessageId::new(i64_to_u64(message_id, "last message id")?),
        role,
        preview,
        truncated,
        created_at_ms,
    }))
}

fn load_session_header(
    connection: &mut Connection,
    session_id: SessionId,
) -> Result<Option<(SessionConfig, SessionStatus, Option<CacheObservation>)>> {
    let mut statement = connection.prepare(
        "SELECT model, status, last_prompt_cache_key, last_stable_prefix_hash, \
                last_request_input_hash, last_request_input_message_count \
         FROM sessions WHERE id = ?1",
    )?;
    statement.bind_i64(1, session_id_i64(session_id)?)?;
    match statement.step()? {
        StepResult::Done => Ok(None),
        StepResult::Row => {
            let model = statement.column_text(0)?;
            let status = parse_session_status(&statement.column_text(1)?)?;
            let last_cache_observation = match (
                statement.column_optional_text(2)?,
                statement.column_optional_i64(3)?,
            ) {
                (Some(prompt_cache_key), Some(stable_prefix_hash)) => Some(CacheObservation {
                    prompt_cache_key,
                    stable_prefix_hash: cache_hash_from_i64(stable_prefix_hash),
                    request_input_hash: statement.column_optional_i64(4)?.map(cache_hash_from_i64),
                    request_input_message_count: statement
                        .column_optional_i64(5)?
                        .map(|count| {
                            i64_to_u64(count, "request input message count").and_then(|count| {
                                count
                                    .try_into()
                                    .context("request input message count exceeds usize range")
                            })
                        })
                        .transpose()?,
                }),
                _ => None,
            };
            Ok(Some((
                SessionConfig::new(model),
                status,
                last_cache_observation,
            )))
        }
    }
}

fn load_session_messages(
    connection: &mut Connection,
    session_id: SessionId,
) -> Result<Vec<AgentMessage>> {
    let mut statement = connection.prepare(
        "SELECT message_id, kind, role, text, item_id, call_id, name, arguments, output, success, \
                first_kept_message_id, tokens_before, details_json \
         FROM messages \
         WHERE session_id = ?1 \
         ORDER BY message_id",
    )?;
    statement.bind_i64(1, session_id_i64(session_id)?)?;

    let mut messages = Vec::new();
    while statement.step()? == StepResult::Row {
        messages.push(decode_message_row(&statement)?);
    }
    Ok(messages)
}

fn decode_message_row(statement: &Statement<'_>) -> Result<AgentMessage> {
    let message_id = MessageId::new(i64_to_u64(statement.column_i64(0), "message id")?);
    let kind = statement.column_text(1)?;
    let item = match kind.as_str() {
        "message" => {
            let role = statement
                .column_optional_text(2)?
                .as_deref()
                .and_then(MessageRole::from_str)
                .context("stored message row has invalid role")?;
            AgentItem::Message {
                role,
                text: statement.column_optional_text(3)?.unwrap_or_default(),
            }
        }
        "function_call" => AgentItem::FunctionCall {
            item_id: statement.column_optional_text(4)?,
            call_id: required_text(statement, 5, "function call id")?,
            name: required_text(statement, 6, "function name")?,
            arguments: statement.column_optional_text(7)?.unwrap_or_default(),
        },
        "function_output" => AgentItem::FunctionOutput {
            call_id: required_text(statement, 5, "function output call id")?,
            output: statement.column_optional_text(8)?.unwrap_or_default(),
            success: statement.column_optional_i64(9)?.unwrap_or(0) != 0,
        },
        "compaction" => {
            let first_kept = statement
                .column_optional_i64(10)?
                .context("stored compaction row is missing first_kept_message_id")?;
            let tokens_before = statement
                .column_optional_i64(11)?
                .context("stored compaction row is missing tokens_before")?;
            let details = match statement.column_optional_text(12)? {
                Some(json) => {
                    serde_json::from_str(&json).context("stored compaction details are invalid")?
                }
                None => CompactionDetails::default(),
            };
            AgentItem::Compaction {
                summary: required_text(statement, 3, "compaction summary")?,
                first_kept_message_id: MessageId::new(i64_to_u64(
                    first_kept,
                    "first kept message id",
                )?),
                tokens_before: i64_to_u64(tokens_before, "compaction tokens_before")?,
                details,
            }
        }
        _ => anyhow::bail!("stored message row has invalid kind {kind:?}"),
    };
    Ok(AgentMessage::from_parts(message_id, item))
}

fn insert_message(
    connection: &mut Connection,
    session_id: SessionId,
    message: &AgentMessage,
    now: i64,
) -> Result<()> {
    let mut values = MessageInsert::new(session_id, message, now)?;
    connection.transaction(|connection| {
        connection.execute(
            "INSERT INTO messages \
             (session_id, message_id, kind, role, text, item_id, call_id, name, arguments, output, success, created_at_ms, first_kept_message_id, tokens_before, details_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            |statement| values.bind(statement),
        )?;
        connection.execute(
            "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
            |statement| {
                statement.bind_i64(1, session_id_i64(session_id)?)?;
                statement.bind_i64(2, now)
            },
        )
    })
}

struct MessageInsert<'a> {
    session_id: SessionId,
    message_id: MessageId,
    kind: &'static str,
    role: Option<&'static str>,
    text: Option<&'a str>,
    item_id: Option<&'a str>,
    call_id: Option<&'a str>,
    name: Option<&'a str>,
    arguments: Option<&'a str>,
    output: Option<&'a str>,
    success: Option<bool>,
    first_kept_message_id: Option<MessageId>,
    tokens_before: Option<u64>,
    details_json: Option<String>,
    created_at_ms: i64,
}

impl<'a> MessageInsert<'a> {
    fn new(session_id: SessionId, message: &'a AgentMessage, created_at_ms: i64) -> Result<Self> {
        let (
            kind,
            role,
            text,
            item_id,
            call_id,
            name,
            arguments,
            output,
            success,
            first_kept_message_id,
            tokens_before,
            details_json,
        ) = match message.item() {
            AgentItem::Message { role, text } => (
                "message",
                Some(role.as_str()),
                Some(text.as_str()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            AgentItem::FunctionCall {
                item_id,
                call_id,
                name,
                arguments,
            } => (
                "function_call",
                None,
                None,
                item_id.as_deref(),
                Some(call_id.as_str()),
                Some(name.as_str()),
                Some(arguments.as_str()),
                None,
                None,
                None,
                None,
                None,
            ),
            AgentItem::FunctionOutput {
                call_id,
                output,
                success,
            } => (
                "function_output",
                None,
                None,
                None,
                Some(call_id.as_str()),
                None,
                None,
                Some(output.as_str()),
                Some(*success),
                None,
                None,
                None,
            ),
            AgentItem::Compaction {
                summary,
                first_kept_message_id,
                tokens_before,
                details,
            } => (
                "compaction",
                None,
                Some(summary.as_str()),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(*first_kept_message_id),
                Some(*tokens_before),
                Some(serde_json::to_string(details)?),
            ),
        };
        Ok(Self {
            session_id,
            message_id: message.id(),
            kind,
            role,
            text,
            item_id,
            call_id,
            name,
            arguments,
            output,
            success,
            first_kept_message_id,
            tokens_before,
            details_json,
            created_at_ms,
        })
    }

    fn bind(&mut self, statement: &mut Statement<'_>) -> Result<()> {
        statement.bind_i64(1, session_id_i64(self.session_id)?)?;
        statement.bind_i64(2, message_id_i64(self.message_id)?)?;
        statement.bind_text(3, self.kind)?;
        statement.bind_optional_text(4, self.role)?;
        statement.bind_optional_text(5, self.text)?;
        statement.bind_optional_text(6, self.item_id)?;
        statement.bind_optional_text(7, self.call_id)?;
        statement.bind_optional_text(8, self.name)?;
        statement.bind_optional_text(9, self.arguments)?;
        statement.bind_optional_text(10, self.output)?;
        match self.success {
            Some(success) => statement.bind_i64(11, i64::from(success))?,
            None => statement.bind_null(11)?,
        }
        statement.bind_i64(12, self.created_at_ms)?;
        statement.bind_optional_i64(
            13,
            self.first_kept_message_id.map(message_id_i64).transpose()?,
        )?;
        statement.bind_optional_i64(14, self.tokens_before.map(u64_to_i64).transpose()?)?;
        statement.bind_optional_text(15, self.details_json.as_deref())
    }
}

#[derive(Debug)]
struct Connection {
    raw: *mut ffi::sqlite3,
}

// SAFETY: SQLite is opened in FULLMUTEX mode and every access is guarded by
// `SessionDatabase`'s mutex.
unsafe impl Send for Connection {}

impl Connection {
    fn open(path: &Path) -> Result<Self> {
        let path = path
            .to_str()
            .with_context(|| format!("database path {} is not UTF-8", path.display()))?;
        let path = CString::new(path).context("database path contains a NUL byte")?;
        let mut raw = ptr::null_mut();
        let flags =
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_FULLMUTEX;
        // SAFETY: `path` is a NUL-terminated CString that lives for the call, and
        // `raw` is a valid out pointer initialized by SQLite.
        let result = unsafe { ffi::sqlite3_open_v2(path.as_ptr(), &mut raw, flags, ptr::null()) };
        if result != ffi::SQLITE_OK {
            let message = if raw.is_null() {
                sqlite_error_string(result)
            } else {
                sqlite_error_message(raw)
            };
            if !raw.is_null() {
                // SAFETY: SQLite returned this handle from `sqlite3_open_v2`; no
                // Rust wrapper owns it on this error path.
                unsafe {
                    ffi::sqlite3_close(raw);
                }
            }
            anyhow::bail!("failed to open session database: {message}");
        }

        let connection = Self { raw };
        // SAFETY: `connection.raw` is a live SQLite connection after the successful
        // open above; both calls only mutate connection-local settings.
        unsafe {
            ffi::sqlite3_extended_result_codes(connection.raw, 1);
            ffi::sqlite3_busy_timeout(connection.raw, BUSY_TIMEOUT_MS);
        }
        Ok(connection)
    }

    fn configure(&mut self) -> Result<()> {
        let pragmas = format!(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = {BUSY_TIMEOUT_MS};
             PRAGMA cache_size = -{SQLITE_CACHE_SIZE_KIB};
             PRAGMA foreign_keys = ON;"
        );
        self.exec(&pragmas)?;
        self.exec(
            "CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY,
                model TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_prompt_cache_key TEXT,
                last_stable_prefix_hash INTEGER,
                last_request_input_hash INTEGER,
                last_request_input_message_count INTEGER
             );
             CREATE TABLE IF NOT EXISTS messages (
                session_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL,
                kind TEXT NOT NULL,
                role TEXT,
                text TEXT,
                item_id TEXT,
                call_id TEXT,
                name TEXT,
                arguments TEXT,
                output TEXT,
                success INTEGER,
                created_at_ms INTEGER NOT NULL,
                first_kept_message_id INTEGER,
                tokens_before INTEGER,
                details_json TEXT,
                PRIMARY KEY (session_id, message_id),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS messages_session_order_idx
                ON messages(session_id, message_id);",
        )?;
        self.ensure_messages_column(
            "first_kept_message_id",
            "ALTER TABLE messages ADD COLUMN first_kept_message_id INTEGER",
        )?;
        self.ensure_messages_column(
            "tokens_before",
            "ALTER TABLE messages ADD COLUMN tokens_before INTEGER",
        )?;
        self.ensure_messages_column(
            "details_json",
            "ALTER TABLE messages ADD COLUMN details_json TEXT",
        )?;
        self.ensure_sessions_column(
            "last_request_input_hash",
            "ALTER TABLE sessions ADD COLUMN last_request_input_hash INTEGER",
        )?;
        self.ensure_sessions_column(
            "last_request_input_message_count",
            "ALTER TABLE sessions ADD COLUMN last_request_input_message_count INTEGER",
        )
    }

    fn ensure_sessions_column(&mut self, column: &str, alter_sql: &str) -> Result<()> {
        if self.table_has_column("sessions", column)? {
            return Ok(());
        }
        self.exec(alter_sql)
    }

    fn ensure_messages_column(&mut self, column: &str, alter_sql: &str) -> Result<()> {
        if self.table_has_column("messages", column)? {
            return Ok(());
        }
        self.exec(alter_sql)
    }

    fn table_has_column(&mut self, table: &str, column: &str) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut statement = self.prepare(&pragma)?;
        while statement.step()? == StepResult::Row {
            if statement.column_text(1)? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn exec(&mut self, sql: &str) -> Result<()> {
        let sql = CString::new(sql).context("SQL contains a NUL byte")?;
        let mut error = ptr::null_mut();
        // SAFETY: `self.raw` is a live connection, `sql` is a NUL-terminated
        // string that lives for the call, and `error` is a valid out pointer.
        let result =
            unsafe { ffi::sqlite3_exec(self.raw, sql.as_ptr(), None, ptr::null_mut(), &mut error) };
        if result == ffi::SQLITE_OK {
            return Ok(());
        }

        let message = if error.is_null() {
            self.error_message()
        } else {
            // SAFETY: On failure, SQLite returns `error` as a valid NUL-terminated
            // message allocated by SQLite until it is freed below.
            let message = unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() };
            // SAFETY: `error` was allocated by SQLite for `sqlite3_exec`.
            unsafe {
                ffi::sqlite3_free(error.cast());
            }
            message
        };
        anyhow::bail!("SQLite exec failed: {message}");
    }

    fn prepare(&mut self, sql: &str) -> Result<Statement<'_>> {
        let sql = CString::new(sql).context("SQL contains a NUL byte")?;
        let mut statement = ptr::null_mut();
        // SAFETY: `self.raw` is a live connection, `sql` is a NUL-terminated
        // statement string, and `statement` is a valid out pointer.
        let result = unsafe {
            ffi::sqlite3_prepare_v2(self.raw, sql.as_ptr(), -1, &mut statement, ptr::null_mut())
        };
        if result != ffi::SQLITE_OK {
            anyhow::bail!("SQLite prepare failed: {}", self.error_message());
        }
        Ok(Statement {
            connection: self,
            raw: statement,
        })
    }

    fn execute(
        &mut self,
        sql: &str,
        bind: impl FnOnce(&mut Statement<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut statement = self.prepare(sql)?;
        bind(&mut statement)?;
        statement.step_done()
    }

    fn transaction<T>(&mut self, action: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.exec("BEGIN IMMEDIATE")?;
        match action(self) {
            Ok(value) => {
                self.exec("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let rollback = self.exec("ROLLBACK");
                if let Err(rollback_error) = rollback {
                    return Err(error).context(format!("rollback also failed: {rollback_error}"));
                }
                Err(error)
            }
        }
    }

    fn changes(&self) -> i32 {
        // SAFETY: `self.raw` is a live SQLite connection owned by `Connection`.
        unsafe { ffi::sqlite3_changes(self.raw) }
    }

    fn error_message(&self) -> String {
        sqlite_error_message(self.raw)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is owned by this `Connection` and is closed exactly
        // once from `Drop`.
        unsafe {
            ffi::sqlite3_close(self.raw);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepResult {
    Row,
    Done,
}

struct Statement<'connection> {
    connection: &'connection mut Connection,
    raw: *mut ffi::sqlite3_stmt,
}

impl Statement<'_> {
    fn bind_i64(&mut self, index: i32, value: i64) -> Result<()> {
        // SAFETY: `self.raw` is a live prepared statement and `index` is checked by
        // SQLite, which reports invalid bindings via the return code.
        self.check_bind(unsafe { ffi::sqlite3_bind_int64(self.raw, index, value) })
    }

    fn bind_text(&mut self, index: i32, value: &str) -> Result<()> {
        let length = value
            .len()
            .try_into()
            .context("bound text is too large for SQLite")?;
        // SAFETY: `value` is valid UTF-8 bytes for `length`, and
        // `SQLITE_TRANSIENT` tells SQLite to copy the bytes before returning.
        self.check_bind(unsafe {
            ffi::sqlite3_bind_text(
                self.raw,
                index,
                value.as_ptr().cast::<c_char>(),
                length,
                ffi::SQLITE_TRANSIENT(),
            )
        })
    }

    fn bind_optional_text(&mut self, index: i32, value: Option<&str>) -> Result<()> {
        match value {
            Some(value) => self.bind_text(index, value),
            None => self.bind_null(index),
        }
    }

    fn bind_optional_i64(&mut self, index: i32, value: Option<i64>) -> Result<()> {
        match value {
            Some(value) => self.bind_i64(index, value),
            None => self.bind_null(index),
        }
    }

    fn bind_null(&mut self, index: i32) -> Result<()> {
        // SAFETY: `self.raw` is a live prepared statement and `index` is checked by
        // SQLite, which reports invalid bindings via the return code.
        self.check_bind(unsafe { ffi::sqlite3_bind_null(self.raw, index) })
    }

    fn check_bind(&self, result: i32) -> Result<()> {
        if result == ffi::SQLITE_OK {
            Ok(())
        } else {
            anyhow::bail!("SQLite bind failed: {}", self.connection.error_message())
        }
    }

    fn step(&mut self) -> Result<StepResult> {
        // SAFETY: `self.raw` is a live prepared statement owned by this wrapper.
        match unsafe { ffi::sqlite3_step(self.raw) } {
            ffi::SQLITE_ROW => Ok(StepResult::Row),
            ffi::SQLITE_DONE => Ok(StepResult::Done),
            _ => anyhow::bail!("SQLite step failed: {}", self.connection.error_message()),
        }
    }

    fn step_done(&mut self) -> Result<()> {
        match self.step()? {
            StepResult::Done => Ok(()),
            StepResult::Row => anyhow::bail!("SQLite statement unexpectedly returned a row"),
        }
    }

    fn column_i64(&self, index: i32) -> i64 {
        // SAFETY: Callers only read columns after `sqlite3_step` returned
        // `SQLITE_ROW`; SQLite validates `index` and returns its default on misuse.
        unsafe { ffi::sqlite3_column_int64(self.raw, index) }
    }

    fn column_optional_i64(&self, index: i32) -> Result<Option<i64>> {
        if self.column_type(index) == ffi::SQLITE_NULL {
            Ok(None)
        } else {
            Ok(Some(self.column_i64(index)))
        }
    }

    fn column_text(&self, index: i32) -> Result<String> {
        self.column_optional_text(index)?
            .with_context(|| format!("column {index} is NULL"))
    }

    fn column_optional_text(&self, index: i32) -> Result<Option<String>> {
        if self.column_type(index) == ffi::SQLITE_NULL {
            return Ok(None);
        }
        // SAFETY: Callers only read columns after `sqlite3_step` returned
        // `SQLITE_ROW`; SQLite keeps the pointer valid until the next step/reset/finalize.
        let text = unsafe { ffi::sqlite3_column_text(self.raw, index) };
        if text.is_null() {
            return Ok(Some(String::new()));
        }
        // SAFETY: `text` is non-null and points at the same SQLite column value.
        let bytes = unsafe { ffi::sqlite3_column_bytes(self.raw, index) };
        let bytes: usize = bytes
            .try_into()
            .context("SQLite returned a negative text length")?;
        // SAFETY: SQLite reports `bytes` as the number of bytes available at
        // `text`; the slice is copied into an owned `String` before the statement advances.
        let slice = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), bytes) };
        Ok(Some(
            std::str::from_utf8(slice)
                .context("stored text is not UTF-8")?
                .to_string(),
        ))
    }

    fn column_type(&self, index: i32) -> i32 {
        // SAFETY: `self.raw` is a live prepared statement; SQLite validates
        // `index` and returns a type code for the current row.
        unsafe { ffi::sqlite3_column_type(self.raw, index) }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is owned by this `Statement` and finalized exactly
        // once from `Drop`.
        unsafe {
            ffi::sqlite3_finalize(self.raw);
        }
    }
}

fn required_text(statement: &Statement<'_>, index: i32, label: &str) -> Result<String> {
    statement
        .column_optional_text(index)?
        .with_context(|| format!("stored row is missing {label}"))
}

fn parse_session_status(value: &str) -> Result<SessionStatus> {
    SessionStatus::from_str(value)
        .with_context(|| format!("invalid stored session status {value:?}"))
}

fn session_id_i64(session_id: SessionId) -> Result<i64> {
    u64_to_i64(session_id.get())
}

fn message_id_i64(message_id: MessageId) -> Result<i64> {
    u64_to_i64(message_id.get())
}

fn u64_to_i64(value: u64) -> Result<i64> {
    value
        .try_into()
        .context("value exceeds SQLite signed integer range")
}

fn usize_to_i64(value: usize, label: &str) -> Result<i64> {
    value
        .try_into()
        .with_context(|| format!("{label} exceeds SQLite signed integer range"))
}

fn i64_to_u64(value: i64, label: &str) -> Result<u64> {
    value
        .try_into()
        .with_context(|| format!("stored {label} is negative"))
}

fn cache_hash_i64(value: u64) -> i64 {
    i64::from_be_bytes(value.to_be_bytes())
}

fn cache_hash_from_i64(value: i64) -> u64 {
    u64::from_be_bytes(value.to_be_bytes())
}

fn now_millis() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?;
    duration
        .as_millis()
        .try_into()
        .context("current time exceeds SQLite signed integer range")
}

fn sqlite_error_message(raw: *mut ffi::sqlite3) -> String {
    // SAFETY: Callers pass a live SQLite connection; SQLite returns a
    // NUL-terminated message pointer valid until the next connection API call.
    unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(raw)) }
        .to_string_lossy()
        .into_owned()
}

fn sqlite_error_string(code: i32) -> String {
    // SAFETY: SQLite returns a static NUL-terminated string for any result code.
    unsafe { CStr::from_ptr(ffi::sqlite3_errstr(code)) }
        .to_string_lossy()
        .into_owned()
}

fn ensure_database_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create database directory {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect database directory {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "database directory {} is not a directory",
        path.display()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = metadata.permissions();
        let mode = permissions.mode();
        if mode & 0o077 != 0 {
            permissions.set_mode(mode & !0o077);
            std::fs::set_permissions(path, permissions).with_context(|| {
                format!("failed to restrict database directory {}", path.display())
            })?;
        }
    }

    Ok(())
}

/// Returns the default SQLite database path.
///
/// # Errors
/// Returns an error when neither `RUST_AGENT_HOME` nor `HOME` is available.
pub(crate) fn default_database_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("RUST_AGENT_HOME") {
        return Ok(PathBuf::from(home).join("sessions.db"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".rust-agent").join("sessions.db"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Instant;

    use crate::bench_support::DurationSummary;

    use super::*;

    #[test]
    fn stores_sessions_and_messages_in_sqlite() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(7);
        database.ensure_session(session_id, "test-model").unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::User,
                        text: "hello".to_string(),
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(2),
                    AgentItem::FunctionOutput {
                        call_id: "call_1".to_string(),
                        output: "ok".to_string(),
                        success: true,
                    },
                ),
            )
            .unwrap();

        let loaded = database.load_session(session_id).unwrap().unwrap();

        assert_eq!(loaded.config.model(), "test-model");
        assert_eq!(loaded.status, SessionStatus::Idle);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].text(), "hello");
        assert_eq!(loaded.messages[1].text(), "ok");
    }

    #[test]
    fn stores_compaction_messages_in_sqlite() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(7);
        let details = CompactionDetails {
            read_files: vec!["Cargo.toml".to_string()],
            modified_files: vec!["src/main.rs".to_string()],
        };
        database.ensure_session(session_id, "test-model").unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(9),
                    AgentItem::Compaction {
                        summary: "checkpoint".to_string(),
                        first_kept_message_id: MessageId::new(5),
                        tokens_before: 1234,
                        details: details.clone(),
                    },
                ),
            )
            .unwrap();

        let loaded = database.load_session(session_id).unwrap().unwrap();

        assert!(matches!(
            loaded.messages[0].item(),
            AgentItem::Compaction {
                summary,
                first_kept_message_id,
                tokens_before,
                details: stored_details,
            } if summary == "checkpoint"
                && *first_kept_message_id == MessageId::new(5)
                && *tokens_before == 1234
                && stored_details == &details
        ));
    }

    #[test]
    fn stores_function_calls_and_session_metadata() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(11);
        database.ensure_session(session_id, "first-model").unwrap();
        database
            .set_session_model(session_id, "second-model")
            .unwrap();
        database
            .set_session_status(session_id, SessionStatus::Failed)
            .unwrap();
        database
            .record_cache_observation(
                session_id,
                &CacheObservation {
                    prompt_cache_key: "cache-key".to_string(),
                    stable_prefix_hash: 0xfedc_ba98_7654_3210,
                    request_input_hash: Some(0x1234),
                    request_input_message_count: Some(2),
                },
            )
            .unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::FunctionCall {
                        item_id: Some("item_1".to_string()),
                        call_id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
                    },
                ),
            )
            .unwrap();

        let loaded = database.load_session(session_id).unwrap().unwrap();

        assert_eq!(loaded.config.model(), "second-model");
        assert_eq!(loaded.status, SessionStatus::Failed);
        assert_eq!(
            loaded.last_cache_observation,
            Some(CacheObservation {
                prompt_cache_key: "cache-key".to_string(),
                stable_prefix_hash: 0xfedc_ba98_7654_3210,
                request_input_hash: Some(0x1234),
                request_input_message_count: Some(2),
            })
        );
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].text(), r#"{"path":"Cargo.toml"}"#);
        assert!(matches!(
            loaded.messages[0].item(),
            AgentItem::FunctionCall {
                item_id: Some(item_id),
                call_id,
                name,
                ..
            } if item_id == "item_1" && call_id == "call_1" && name == "read_file"
        ));
    }

    #[test]
    fn lists_session_metadata_with_latest_message_preview() {
        let database = SessionDatabase::in_memory().unwrap();
        let first = SessionId::new(7);
        let second = SessionId::new(8);
        database.ensure_session(first, "first-model").unwrap();
        database
            .insert_message(
                first,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::User,
                        text: "older prompt".to_string(),
                    },
                ),
            )
            .unwrap();
        database.ensure_session(second, "second-model").unwrap();
        database
            .set_session_status(second, SessionStatus::Failed)
            .unwrap();
        database
            .insert_message(
                second,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::FunctionOutput {
                        call_id: "call_1".to_string(),
                        output: "tool output".to_string(),
                        success: true,
                    },
                ),
            )
            .unwrap();
        database
            .insert_message(
                second,
                &AgentMessage::from_parts(
                    MessageId::new(2),
                    AgentItem::Message {
                        role: MessageRole::Assistant,
                        text: "x".repeat(MAX_SESSION_PREVIEW_CHARS as usize + 8),
                    },
                ),
            )
            .unwrap();

        let sessions = database.list_session_metadata(0, 10).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, second);
        assert_eq!(sessions[0].model, "second-model");
        assert_eq!(sessions[0].status, SessionStatus::Failed);
        assert_eq!(sessions[0].message_count, 2);
        let last_message = sessions[0].last_message.as_ref().unwrap();
        assert_eq!(last_message.message_id, MessageId::new(2));
        assert_eq!(last_message.role, MessageRole::Assistant);
        assert_eq!(
            last_message.preview.len(),
            MAX_SESSION_PREVIEW_CHARS as usize
        );
        assert!(last_message.truncated);

        let paged = database.list_session_metadata(1, 1).unwrap();
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].session_id, first);
        assert_eq!(
            paged[0].last_message.as_ref().unwrap().preview,
            "older prompt"
        );
    }

    #[test]
    fn loads_single_session_metadata() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(12);
        database.ensure_session(session_id, "test-model").unwrap();

        let metadata = database.session_metadata(session_id).unwrap().unwrap();

        assert_eq!(metadata.session_id, session_id);
        assert_eq!(metadata.model, "test-model");
        assert_eq!(metadata.status, SessionStatus::Idle);
        assert_eq!(metadata.message_count, 0);
        assert!(metadata.last_message.is_none());
        assert!(database
            .session_metadata(SessionId::new(99))
            .unwrap()
            .is_none());
    }

    #[test]
    fn deletes_single_messages_without_removing_session() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(13);
        database.ensure_session(session_id, "test-model").unwrap();
        for id in 1..=3 {
            database
                .insert_message(
                    session_id,
                    &AgentMessage::from_parts(
                        MessageId::new(id),
                        AgentItem::Message {
                            role: MessageRole::User,
                            text: format!("message {id}"),
                        },
                    ),
                )
                .unwrap();
        }

        database
            .delete_message(session_id, MessageId::new(2))
            .unwrap();
        let loaded = database.load_session(session_id).unwrap().unwrap();

        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].text(), "message 1");
        assert_eq!(loaded.messages[1].text(), "message 3");
    }

    #[test]
    fn deletes_sessions_with_messages() {
        let database = SessionDatabase::in_memory().unwrap();
        let session_id = SessionId::new(9);
        database.ensure_session(session_id, "test-model").unwrap();
        database
            .insert_message(
                session_id,
                &AgentMessage::from_parts(
                    MessageId::new(1),
                    AgentItem::Message {
                        role: MessageRole::Assistant,
                        text: "hi".to_string(),
                    },
                ),
            )
            .unwrap();

        assert!(database.delete_session(session_id).unwrap());
        assert!(database.load_session(session_id).unwrap().is_none());
    }

    #[test]
    #[ignore = "release-mode SQLite session storage benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_sqlite_session_database_large_history() {
        const MESSAGES: u64 = 2_000;
        const LOAD_SAMPLES: usize = 15;

        let (temp_dir, database_path) = temp_database_path("large-history");
        fs::create_dir_all(&temp_dir).unwrap();
        let database = SessionDatabase::open(&database_path).unwrap();
        let session_id = SessionId::new(21);
        database.ensure_session(session_id, "bench-model").unwrap();

        let write_started = Instant::now();
        for id in 1..=MESSAGES {
            let role = if id % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            database
                .insert_message(
                    session_id,
                    &AgentMessage::from_parts(
                        MessageId::new(id),
                        AgentItem::Message {
                            role,
                            text: format!("benchmark message {id} {}", "x".repeat(96)),
                        },
                    ),
                )
                .unwrap();
        }
        let write_elapsed = write_started.elapsed();

        let mut load_samples = Vec::with_capacity(LOAD_SAMPLES);
        let mut loaded_messages = 0usize;
        for _ in 0..LOAD_SAMPLES {
            let started = Instant::now();
            let loaded = database.load_session(session_id).unwrap().unwrap();
            let elapsed = started.elapsed();

            loaded_messages = loaded.messages.len();
            std::hint::black_box(loaded);
            load_samples.push(elapsed);
        }

        let load = DurationSummary::from_samples(&mut load_samples);
        println!(
            "sqlite_session_database_large_history messages={MESSAGES} loaded_messages={loaded_messages} load_samples={LOAD_SAMPLES} write_ms={:.3} write_messages_per_sec={:.0} load_min_ms={:.3} load_median_ms={:.3} load_max_ms={:.3}",
            crate::bench_support::duration_ms(write_elapsed),
            MESSAGES as f64 / write_elapsed.as_secs_f64(),
            load.min_ms(),
            load.median_ms(),
            load.max_ms(),
        );

        fs::remove_dir_all(temp_dir).unwrap();
    }

    fn temp_database_path(label: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rust-agent-storage-{label}-{}-{}",
            std::process::id(),
            unique_nanos()
        ));
        let path = dir.join("sessions.db");
        (dir, path)
    }

    fn unique_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
