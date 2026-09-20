//! Bounded SQLx pool configuration for Postgres.
//!
//! Connection strings stay in the environment and are never printed. The pool
//! is constructed explicitly by callers; replica processes do not apply
//! migrations on startup. Builds use runtime SQL and do not need a live
//! database or SQLx offline metadata.

use crate::core::config::SecretString;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Executor, PgPool};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// Default pool size for the gateway runtime role.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;
/// Hard cap on pool size.
pub const MAX_MAX_CONNECTIONS: u32 = 32;
/// Default time to wait for a pool connection.
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(3_000);
/// Default per-session statement timeout applied after connect.
pub const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Sanitized database configuration failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseConfigError {
    /// `DATABASE_URL` is missing or blank.
    MissingUrl,
    /// The URL cannot be parsed without exposing it.
    InvalidUrl,
    /// A numeric pool bound is missing, zero, or out of range.
    InvalidPoolBound,
}

impl DatabaseConfigError {
    /// Returns the stable diagnostic token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingUrl => "missing_database_url",
            Self::InvalidUrl => "invalid_database_url",
            Self::InvalidPoolBound => "invalid_database_pool_bound",
        }
    }
}

impl fmt::Display for DatabaseConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUrl => {
                f.write_str("DATABASE_URL is required and must be nonempty")
            }
            Self::InvalidUrl => {
                f.write_str("DATABASE_URL must be a PostgreSQL URL without being logged")
            }
            Self::InvalidPoolBound => f.write_str("database pool bounds are invalid"),
        }
    }
}

impl std::error::Error for DatabaseConfigError {}

/// Validated pool settings. `Debug` never prints the connection string.
pub struct DatabasePoolConfig {
    url: SecretString,
    max_connections: u32,
    acquire_timeout: Duration,
    statement_timeout: Duration,
}

impl fmt::Debug for DatabasePoolConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabasePoolConfig")
            .field("url", &"[redacted]")
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout_ms", &self.acquire_timeout.as_millis())
            .field("statement_timeout_ms", &self.statement_timeout.as_millis())
            .finish()
    }
}

impl DatabasePoolConfig {
    /// Builds pool settings from a PostgreSQL URL.
    ///
    /// # Parameters
    /// - `url` - Environment-only connection string
    ///
    /// # Returns
    /// Validated settings with documented defaults
    ///
    /// # Errors
    /// Returns `MissingUrl` or `InvalidUrl` without echoing the value.
    pub fn new(url: impl Into<String>) -> Result<Self, DatabaseConfigError> {
        let url = url.into();
        if url.trim().is_empty() {
            return Err(DatabaseConfigError::MissingUrl);
        }
        let _ = PgConnectOptions::from_str(url.trim())
            .map_err(|_| DatabaseConfigError::InvalidUrl)?;
        Ok(Self {
            url: SecretString::new(url),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            statement_timeout: DEFAULT_STATEMENT_TIMEOUT,
        })
    }

    /// Loads pool settings from the process environment.
    ///
    /// Reads `DATABASE_URL` and optional `OPMUX_DB_MAX_CONNECTIONS`,
    /// `OPMUX_DB_ACQUIRE_TIMEOUT_MS`, and `OPMUX_DB_STATEMENT_TIMEOUT_MS`.
    /// Gateway startup does not yet require this; persistence tests do.
    ///
    /// # Errors
    /// Returns a sanitized error when the URL or numeric bounds are invalid.
    pub fn from_env() -> Result<Self, DatabaseConfigError> {
        let url =
            std::env::var("DATABASE_URL").map_err(|_| DatabaseConfigError::MissingUrl)?;
        let mut config = Self::new(url)?;
        if let Some(raw) = std::env::var_os("OPMUX_DB_MAX_CONNECTIONS") {
            let value = parse_u32(&raw).ok_or(DatabaseConfigError::InvalidPoolBound)?;
            config = config.with_max_connections(value)?;
        }
        if let Some(raw) = std::env::var_os("OPMUX_DB_ACQUIRE_TIMEOUT_MS") {
            let value = parse_u32(&raw).ok_or(DatabaseConfigError::InvalidPoolBound)?;
            config.acquire_timeout = bounded_timeout_ms(value)?;
        }
        if let Some(raw) = std::env::var_os("OPMUX_DB_STATEMENT_TIMEOUT_MS") {
            let value = parse_u32(&raw).ok_or(DatabaseConfigError::InvalidPoolBound)?;
            config.statement_timeout = bounded_timeout_ms(value)?;
        }
        Ok(config)
    }

    /// Overrides the pool size within the documented bound.
    ///
    /// # Parameters
    /// - `max_connections` - Maximum connections, 1 through 32
    ///
    /// # Errors
    /// Returns `InvalidPoolBound` when the value is outside 1..=32.
    pub fn with_max_connections(
        mut self,
        max_connections: u32,
    ) -> Result<Self, DatabaseConfigError> {
        if max_connections == 0 || max_connections > MAX_MAX_CONNECTIONS {
            return Err(DatabaseConfigError::InvalidPoolBound);
        }
        self.max_connections = max_connections;
        Ok(self)
    }

    /// Returns the configured maximum connections.
    pub fn max_connections(&self) -> u32 {
        self.max_connections
    }

    /// Overrides the acquire timeout (100ms through 60s).
    pub fn with_acquire_timeout(
        mut self,
        acquire_timeout: Duration,
    ) -> Result<Self, DatabaseConfigError> {
        let ms = u32::try_from(acquire_timeout.as_millis())
            .map_err(|_| DatabaseConfigError::InvalidPoolBound)?;
        self.acquire_timeout = bounded_timeout_ms(ms)?;
        Ok(self)
    }

    /// Connects a Tokio Postgres pool with bounded acquire and statement timeouts.
    ///
    /// Hosted deployments should use a direct or session-pooler URL with
    /// `sslmode=verify-full`. Transaction-mode poolers are incompatible with
    /// SQLx prepared statements. Local loopback may use `sslmode=disable`.
    ///
    /// # Returns
    /// A shared `PgPool`
    ///
    /// # Errors
    /// Returns `InvalidUrl` when options cannot be built, or `Unavailable`
    /// equivalents are left to the caller via `sqlx::Error` mapped by stores.
    pub async fn connect(&self) -> Result<PgPool, sqlx::Error> {
        self.connect_with_startup_sql(None).await
    }

    /// Connects a pool and runs `SET ROLE` on each session.
    ///
    /// Used by tests to exercise runtime versus operator grants. The role name
    /// must be a trusted identifier; it is not accepted from request input.
    ///
    /// # Parameters
    /// - `role` - Existing Postgres role to assume after connect
    pub async fn connect_with_role(&self, role: &str) -> Result<PgPool, sqlx::Error> {
        validate_role_name(role)?;
        let sql = format!("SET ROLE {role}");
        self.connect_with_startup_sql(Some(sql)).await
    }

    async fn connect_with_startup_sql(
        &self,
        extra: Option<String>,
    ) -> Result<PgPool, sqlx::Error> {
        let statement_timeout_ms = self.statement_timeout.as_millis().to_string();
        let options = PgConnectOptions::from_str(self.url.expose())?
            .application_name("opmux-gateway")
            .options([("statement_timeout", statement_timeout_ms.as_str())]);
        let max_connections = self.max_connections;
        let acquire_timeout = self.acquire_timeout;
        let mut pool_options = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout);
        if let Some(role_sql) = extra {
            pool_options = pool_options.after_connect(move |conn, _meta| {
                let role_sql = role_sql.clone();
                Box::pin(async move {
                    conn.execute(role_sql.as_str()).await?;
                    Ok(())
                })
            });
        }
        pool_options.connect_with(options).await
    }
}

fn parse_u32(raw: &std::ffi::OsString) -> Option<u32> {
    raw.to_str()?.parse().ok()
}

fn bounded_timeout_ms(value: u32) -> Result<Duration, DatabaseConfigError> {
    if !(100..=60_000).contains(&value) {
        return Err(DatabaseConfigError::InvalidPoolBound);
    }
    Ok(Duration::from_millis(u64::from(value)))
}

fn validate_role_name(role: &str) -> Result<(), sqlx::Error> {
    if role.is_empty() || !role.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') {
        return Err(sqlx::Error::Protocol("invalid session role name".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_blank_url() {
        assert_eq!(
            DatabasePoolConfig::new("  ").err(),
            Some(DatabaseConfigError::MissingUrl)
        );
    }

    #[test]
    fn new_rejects_unparseable_url_without_echoing_it() {
        let err = DatabasePoolConfig::new("not-a-postgres-url").unwrap_err();
        assert_eq!(err, DatabaseConfigError::InvalidUrl);
        assert!(!err.to_string().contains("not-a-postgres-url"));
    }

    #[test]
    fn debug_redacts_connection_string() {
        let config = DatabasePoolConfig::new(
            "postgresql://opmux:s3cret-fixture@127.0.0.1:55432/postgres",
        )
        .expect("parseable url");
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("s3cret-fixture"));
        assert!(!rendered.contains("opmux:"));
    }

    #[test]
    fn max_connections_are_bounded() {
        let config = DatabasePoolConfig::new("postgres://127.0.0.1/postgres")
            .unwrap()
            .with_max_connections(2)
            .unwrap();
        assert_eq!(config.max_connections(), 2);
        assert_eq!(
            DatabasePoolConfig::new("postgres://127.0.0.1/postgres")
                .unwrap()
                .with_max_connections(0)
                .err(),
            Some(DatabaseConfigError::InvalidPoolBound)
        );
        assert_eq!(
            DatabasePoolConfig::new("postgres://127.0.0.1/postgres")
                .unwrap()
                .with_max_connections(33)
                .err(),
            Some(DatabaseConfigError::InvalidPoolBound)
        );
    }

    #[test]
    fn error_tokens_are_stable_and_secret_free() {
        assert_eq!(
            DatabaseConfigError::MissingUrl.as_str(),
            "missing_database_url"
        );
        assert_eq!(
            DatabaseConfigError::InvalidUrl.as_str(),
            "invalid_database_url"
        );
    }
}
