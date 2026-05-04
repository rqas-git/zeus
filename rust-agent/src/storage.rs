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

const BUSY_TIMEOUT_MS: i32 = 5_000;

/// Durable state for one loaded session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredSession {
    pub(crate) config: SessionConfig,
    pub(crate) status: SessionStatus,
    pub(crate) messages: Vec<AgentMessage>,
    pub(crate) last_cache_observation: Option<CacheObservation>,
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
                 SET last_prompt_cache_key = ?2, last_stable_prefix_hash = ?3, updated_at_ms = ?4 \
                 WHERE id = ?1",
                |statement| {
                    statement.bind_i64(1, session_id_i64(session_id)?)?;
                    statement.bind_text(2, &observation.prompt_cache_key)?;
                    statement.bind_i64(3, u64_to_i64(observation.stable_prefix_hash)?)?;
                    statement.bind_i64(4, now)
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

fn load_session_header(
    connection: &mut Connection,
    session_id: SessionId,
) -> Result<Option<(SessionConfig, SessionStatus, Option<CacheObservation>)>> {
    let mut statement = connection.prepare(
        "SELECT model, status, last_prompt_cache_key, last_stable_prefix_hash \
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
                    stable_prefix_hash: i64_to_u64(stable_prefix_hash, "stable prefix hash")?,
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
        "SELECT message_id, kind, role, text, item_id, call_id, name, arguments, output, success \
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
             (session_id, message_id, kind, role, text, item_id, call_id, name, arguments, output, success, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
    created_at_ms: i64,
}

impl<'a> MessageInsert<'a> {
    fn new(session_id: SessionId, message: &'a AgentMessage, created_at_ms: i64) -> Result<Self> {
        let (kind, role, text, item_id, call_id, name, arguments, output, success) =
            match message.item() {
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
        statement.bind_i64(12, self.created_at_ms)
    }
}

#[derive(Debug)]
struct Connection {
    raw: *mut ffi::sqlite3,
}

// SQLite is opened in FULLMUTEX mode and every access is guarded by `SessionDatabase`'s mutex.
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
        let result = unsafe { ffi::sqlite3_open_v2(path.as_ptr(), &mut raw, flags, ptr::null()) };
        if result != ffi::SQLITE_OK {
            let message = if raw.is_null() {
                sqlite_error_string(result)
            } else {
                sqlite_error_message(raw)
            };
            if !raw.is_null() {
                unsafe {
                    ffi::sqlite3_close(raw);
                }
            }
            anyhow::bail!("failed to open session database: {message}");
        }

        let connection = Self { raw };
        unsafe {
            ffi::sqlite3_extended_result_codes(connection.raw, 1);
            ffi::sqlite3_busy_timeout(connection.raw, BUSY_TIMEOUT_MS);
        }
        Ok(connection)
    }

    fn configure(&mut self) -> Result<()> {
        self.exec(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA cache_size = -64000;
             PRAGMA foreign_keys = ON;",
        )?;
        self.exec(
            "CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY,
                model TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_prompt_cache_key TEXT,
                last_stable_prefix_hash INTEGER
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
                PRIMARY KEY (session_id, message_id),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS messages_session_order_idx
                ON messages(session_id, message_id);",
        )
    }

    fn exec(&mut self, sql: &str) -> Result<()> {
        let sql = CString::new(sql).context("SQL contains a NUL byte")?;
        let mut error = ptr::null_mut();
        let result =
            unsafe { ffi::sqlite3_exec(self.raw, sql.as_ptr(), None, ptr::null_mut(), &mut error) };
        if result == ffi::SQLITE_OK {
            return Ok(());
        }

        let message = if error.is_null() {
            self.error_message()
        } else {
            let message = unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() };
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
        unsafe { ffi::sqlite3_changes(self.raw) }
    }

    fn error_message(&self) -> String {
        sqlite_error_message(self.raw)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
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
        self.check_bind(unsafe { ffi::sqlite3_bind_int64(self.raw, index, value) })
    }

    fn bind_text(&mut self, index: i32, value: &str) -> Result<()> {
        let length = value
            .len()
            .try_into()
            .context("bound text is too large for SQLite")?;
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

    fn bind_null(&mut self, index: i32) -> Result<()> {
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
        let text = unsafe { ffi::sqlite3_column_text(self.raw, index) };
        if text.is_null() {
            return Ok(Some(String::new()));
        }
        let bytes = unsafe { ffi::sqlite3_column_bytes(self.raw, index) };
        let bytes: usize = bytes
            .try_into()
            .context("SQLite returned a negative text length")?;
        let slice = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), bytes) };
        Ok(Some(
            std::str::from_utf8(slice)
                .context("stored text is not UTF-8")?
                .to_string(),
        ))
    }

    fn column_type(&self, index: i32) -> i32 {
        unsafe { ffi::sqlite3_column_type(self.raw, index) }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
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

fn i64_to_u64(value: i64, label: &str) -> Result<u64> {
    value
        .try_into()
        .with_context(|| format!("stored {label} is negative"))
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
    unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(raw)) }
        .to_string_lossy()
        .into_owned()
}

fn sqlite_error_string(code: i32) -> String {
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
}
