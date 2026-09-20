//! SQLx Postgres implementation of [`AuthStore`].

use super::error::AuthStoreError;
use super::models::{
    ApiKeyKind, ApiKeyRecord, ClientRecord, KeyDigest, NewApiKey, NewClient,
    RevokeOutcome, MAX_KEY_LIST_LIMIT,
};
use super::store::AuthStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const CLIENT_COLUMNS: &str = "id, display_name, created_at";
const KEY_COLUMNS: &str = "id, client_id, key_digest, display_id, name, kind, \
     created_at, last_used_at, revoked_at";

/// Authentication store backed by `opmux_private` tables.
pub struct PostgresAuthStore {
    pool: PgPool,
}

impl PostgresAuthStore {
    /// Wraps an existing pool. The caller chooses the connected role.
    ///
    /// # Parameters
    /// - `pool` - Bounded SQLx pool
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns a clone of the underlying pool.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

#[async_trait]
impl AuthStore for PostgresAuthStore {
    async fn provision_client_with_key(
        &self,
        client: NewClient,
        key: NewApiKey,
    ) -> Result<(ClientRecord, ApiKeyRecord), AuthStoreError> {
        if key.client_id != client.id {
            return Err(AuthStoreError::UnknownClient);
        }
        let mut tx = self.pool.begin().await.map_err(AuthStoreError::from_sqlx)?;
        let client_record = match insert_client(&mut tx, &client).await {
            Ok(record) => record,
            Err(err) => return rollback_err(tx, err).await,
        };
        let key_record = match insert_key(&mut tx, &key).await {
            Ok(record) => record,
            Err(err) => return rollback_err(tx, err).await,
        };
        tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
        Ok((client_record, key_record))
    }

    async fn insert_key(&self, key: NewApiKey) -> Result<ApiKeyRecord, AuthStoreError> {
        let mut tx = self.pool.begin().await.map_err(AuthStoreError::from_sqlx)?;
        let record = match insert_key(&mut tx, &key).await {
            Ok(record) => record,
            Err(err) => return rollback_err(tx, err).await,
        };
        tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
        Ok(record)
    }

    async fn find_key_by_digest(
        &self,
        digest: &KeyDigest,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        let sql = format!(
            "SELECT {KEY_COLUMNS} FROM opmux_private.api_keys WHERE key_digest = $1"
        );
        let row = sqlx::query(&sql)
            .bind(digest.as_bytes().as_slice())
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthStoreError::from_sqlx)?;
        row.map(|row| api_key_from_row(&row)).transpose()
    }

    async fn authenticate_digest(
        &self,
        digest: &KeyDigest,
        used_at: DateTime<Utc>,
    ) -> Result<Option<ApiKeyRecord>, AuthStoreError> {
        let mut tx = self.pool.begin().await.map_err(AuthStoreError::from_sqlx)?;
        let row = match sqlx::query(&format!(
            "SELECT {KEY_COLUMNS}
             FROM opmux_private.api_keys
             WHERE key_digest = $1
             FOR UPDATE"
        ))
        .bind(digest.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => return rollback_err(tx, AuthStoreError::from_sqlx(err)).await,
        };
        let Some(row) = row else {
            tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
            return Ok(None);
        };
        let mut record = match api_key_from_row(&row) {
            Ok(record) => record,
            Err(err) => return rollback_err(tx, err).await,
        };
        if record.is_revoked() {
            tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
            return Ok(Some(record));
        }
        let updated = match sqlx::query(
            "UPDATE opmux_private.api_keys
             SET last_used_at = $3
             WHERE id = $1
               AND client_id = $2
               AND revoked_at IS NULL
               AND (last_used_at IS NULL OR last_used_at < $3)
             RETURNING last_used_at",
        )
        .bind(record.id)
        .bind(record.client_id)
        .bind(used_at)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => return rollback_err(tx, AuthStoreError::from_sqlx(err)).await,
        };
        if let Some(updated) = updated {
            record.last_used_at = match updated.try_get("last_used_at") {
                Ok(value) => value,
                Err(err) => {
                    return rollback_err(tx, AuthStoreError::from_sqlx(err)).await;
                }
            };
        }
        tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
        Ok(Some(record))
    }

    async fn touch_last_used(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        used_at: DateTime<Utc>,
    ) -> Result<bool, AuthStoreError> {
        let result = sqlx::query(
            "UPDATE opmux_private.api_keys
             SET last_used_at = $3
             WHERE id = $1
               AND client_id = $2
               AND revoked_at IS NULL
               AND (last_used_at IS NULL OR last_used_at < $3)",
        )
        .bind(key_id)
        .bind(client_id)
        .bind(used_at)
        .execute(&self.pool)
        .await
        .map_err(AuthStoreError::from_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    async fn list_keys_for_client(
        &self,
        client_id: Uuid,
        limit: i64,
        offset: i64,
        kind: Option<ApiKeyKind>,
    ) -> Result<Vec<ApiKeyRecord>, AuthStoreError> {
        if !(1..=MAX_KEY_LIST_LIMIT).contains(&limit) || offset < 0 {
            return Err(AuthStoreError::InvalidLimit);
        }
        let sql = format!(
            "SELECT {KEY_COLUMNS}
             FROM opmux_private.api_keys
             WHERE client_id = $1
               AND ($4::text IS NULL OR kind = $4)
             ORDER BY created_at DESC, id DESC
             LIMIT $2 OFFSET $3"
        );
        let rows = sqlx::query(&sql)
            .bind(client_id)
            .bind(limit)
            .bind(offset)
            .bind(kind.map(ApiKeyKind::as_str))
            .fetch_all(&self.pool)
            .await
            .map_err(AuthStoreError::from_sqlx)?;
        rows.iter().map(api_key_from_row).collect()
    }

    async fn revoke_key(
        &self,
        client_id: Uuid,
        key_id: Uuid,
        revoked_at: DateTime<Utc>,
    ) -> Result<RevokeOutcome, AuthStoreError> {
        let mut tx = self.pool.begin().await.map_err(AuthStoreError::from_sqlx)?;
        let updated = match sqlx::query(&format!(
            "UPDATE opmux_private.api_keys
             SET revoked_at = $3
             WHERE id = $1 AND client_id = $2 AND revoked_at IS NULL
             RETURNING {KEY_COLUMNS}"
        ))
        .bind(key_id)
        .bind(client_id)
        .bind(revoked_at)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => return rollback_err(tx, AuthStoreError::from_sqlx(err)).await,
        };
        if let Some(row) = updated {
            let record = match api_key_from_row(&row) {
                Ok(record) => record,
                Err(err) => return rollback_err(tx, err).await,
            };
            tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
            return Ok(RevokeOutcome::Revoked(record));
        }
        let existing = match sqlx::query(&format!(
            "SELECT {KEY_COLUMNS}
             FROM opmux_private.api_keys
             WHERE id = $1 AND client_id = $2"
        ))
        .bind(key_id)
        .bind(client_id)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => return rollback_err(tx, AuthStoreError::from_sqlx(err)).await,
        };
        tx.commit().await.map_err(AuthStoreError::from_sqlx)?;
        match existing {
            Some(row) => Ok(RevokeOutcome::AlreadyRevoked(api_key_from_row(&row)?)),
            None => Ok(RevokeOutcome::NotFound),
        }
    }
}

async fn rollback_err<T>(
    tx: Transaction<'_, Postgres>,
    err: AuthStoreError,
) -> Result<T, AuthStoreError> {
    let _ = tx.rollback().await;
    Err(err)
}

async fn insert_client(
    tx: &mut Transaction<'_, Postgres>,
    client: &NewClient,
) -> Result<ClientRecord, AuthStoreError> {
    let sql = format!(
        "INSERT INTO opmux_private.clients (id, display_name, created_at)
         VALUES ($1, $2, $3)
         RETURNING {CLIENT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(client.id)
        .bind(&client.display_name)
        .bind(client.created_at)
        .fetch_one(&mut **tx)
        .await
        .map_err(AuthStoreError::from_sqlx)?;
    client_from_row(&row)
}

async fn insert_key(
    tx: &mut Transaction<'_, Postgres>,
    key: &NewApiKey,
) -> Result<ApiKeyRecord, AuthStoreError> {
    let sql = format!(
        "INSERT INTO opmux_private.api_keys (
             id, client_id, key_digest, display_id, name, kind, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING {KEY_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(key.id)
        .bind(key.client_id)
        .bind(key.digest.as_bytes().as_slice())
        .bind(&key.display_id)
        .bind(&key.name)
        .bind(key.kind.as_str())
        .bind(key.created_at)
        .fetch_one(&mut **tx)
        .await
        .map_err(AuthStoreError::from_sqlx)?;
    api_key_from_row(&row)
}

fn client_from_row(row: &PgRow) -> Result<ClientRecord, AuthStoreError> {
    Ok(ClientRecord {
        id: row.try_get("id").map_err(AuthStoreError::from_sqlx)?,
        display_name: row
            .try_get("display_name")
            .map_err(AuthStoreError::from_sqlx)?,
        created_at: row
            .try_get("created_at")
            .map_err(AuthStoreError::from_sqlx)?,
    })
}

fn api_key_from_row(row: &PgRow) -> Result<ApiKeyRecord, AuthStoreError> {
    let digest = row
        .try_get::<Vec<u8>, _>("key_digest")
        .map_err(AuthStoreError::from_sqlx)?;
    let kind = row
        .try_get::<String, _>("kind")
        .map_err(AuthStoreError::from_sqlx)?;
    Ok(ApiKeyRecord {
        id: row.try_get("id").map_err(AuthStoreError::from_sqlx)?,
        client_id: row
            .try_get("client_id")
            .map_err(AuthStoreError::from_sqlx)?,
        digest: KeyDigest::from_slice(&digest)?,
        display_id: row
            .try_get("display_id")
            .map_err(AuthStoreError::from_sqlx)?,
        name: row.try_get("name").map_err(AuthStoreError::from_sqlx)?,
        kind: ApiKeyKind::parse(&kind).map_err(AuthStoreError::from_sqlx)?,
        created_at: row
            .try_get("created_at")
            .map_err(AuthStoreError::from_sqlx)?,
        last_used_at: row
            .try_get("last_used_at")
            .map_err(AuthStoreError::from_sqlx)?,
        revoked_at: row
            .try_get("revoked_at")
            .map_err(AuthStoreError::from_sqlx)?,
    })
}
