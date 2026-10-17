//! KeyStore-owned atomic enrollment; no public SQL capability.
use super::{
    generate_key, hash_key, DateTime, KeyRow, KeyStore, KeyStoreError, KeyUsage, Pool, Row, Sqlite,
    Utc, VirtualKey,
};
use crate::enrollment::{
    Administrator, DeviceCredential, EnrollmentError as E, Invitation, InviteRequest,
};

pub(super) async fn migrate(pool: &Pool<Sqlite>) -> Result<(), KeyStoreError> {
    sqlx::query("CREATE TABLE IF NOT EXISTS device_enrollment_v1 (token_hash TEXT PRIMARY KEY, template_hash TEXT NOT NULL, authority TEXT NOT NULL, expires_at INTEGER NOT NULL, device_ttl INTEGER NOT NULL, consumed INTEGER NOT NULL DEFAULT 0)")
        .execute(pool).await.map_err(|e| KeyStoreError::Database(e.to_string()))?;
    Ok(())
}
// Bind original persisted bytes, not reserialized HashSets (random order).
fn snapshot(k: &KeyRow) -> Result<String, E> {
    serde_json::to_string(&serde_json::json!({"permissions":k.permissions,"owner":k.owner,"oauth_user":k.oauth_user,"oauth_account":k.oauth_account,"valid_after":k.valid_after,"expires_at":k.expires_at})).map_err(|_| E::Unavailable)
}
fn eligible(k: &VirtualKey, now: DateTime<Utc>) -> bool {
    let p = &k.permissions;
    !k.disabled
        && k.expires_at.is_none_or(|t| t > now)
        && k.valid_after.is_none_or(|t| t <= now)
        && p.rpm_limit.is_none()
        && p.tpm_limit.is_none()
        && p.rpd_limit.is_none()
        && p.max_budget_usd.is_none()
        && p.budget_window.is_none()
}
impl KeyStore {
    pub(super) async fn init_enrollment(&self) -> Result<(), KeyStoreError> {
        migrate(&self.pool).await
    }

    pub async fn invite_device(
        &self,
        _admin: &Administrator,
        request: InviteRequest,
    ) -> Result<Invitation, E> {
        if request.template_id.len() > 128
            || !(1..=600).contains(&request.invitation_ttl_seconds)
            || !(1..=31_536_000).contains(&request.device_ttl_seconds)
        {
            return Err(E::Invalid);
        }
        let now = Utc::now();
        let row: KeyRow = sqlx::query_as("SELECT * FROM virtual_keys WHERE key_id=?")
            .bind(&request.template_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| E::Unavailable)?
            .ok_or(E::Invalid)?;
        let k = row.to_virtual_key().map_err(|_| E::Unavailable)?;
        if !eligible(&k, now) {
            return Err(E::Invalid);
        }
        let authority = snapshot(&row)?;
        let expires_at = (now
            + chrono::Duration::seconds(i64::from(request.invitation_ttl_seconds)))
        .timestamp();
        let token = format!(
            "enroll_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query("INSERT INTO device_enrollment_v1(token_hash,template_hash,authority,expires_at,device_ttl) VALUES(?,?,?,?,?)")
            .bind(hash_key(&token)).bind(k.key_hash).bind(authority).bind(expires_at).bind(i64::from(request.device_ttl_seconds))
            .execute(&self.pool).await.map_err(|_| E::Unavailable)?;
        Ok(Invitation { token, expires_at })
    }

    pub async fn revoke_device_invitation(
        &self,
        _admin: &Administrator,
        token: &str,
    ) -> Result<(), E> {
        if token.len() != 71 || !token.starts_with("enroll_") {
            return Err(E::Invalid);
        }
        sqlx::query("UPDATE device_enrollment_v1 SET consumed=1 WHERE token_hash=?")
            .bind(hash_key(token))
            .execute(&self.pool)
            .await
            .map_err(|_| E::Unavailable)?;
        Ok(())
    }

    pub async fn redeem_device(&self, token: &str) -> Result<DeviceCredential, E> {
        if token.len() != 71 || !token.starts_with("enroll_") {
            return Err(E::Invalid);
        }
        let mut tx = self.pool.begin().await.map_err(|_| E::Unavailable)?;
        // First statement is a write: serialize independent SQLite connections,
        // avoiding a read-to-write transaction upgrade race.
        let row = sqlx::query("UPDATE device_enrollment_v1 SET consumed=1 WHERE token_hash=? AND consumed=0 AND expires_at>CAST(strftime('%s','now') AS INTEGER) RETURNING template_hash,authority,device_ttl")
            .bind(hash_key(token)).fetch_optional(&mut *tx).await.map_err(|_| E::Unavailable)?.ok_or(E::Invalid)?;
        let parent: String = row.try_get("template_hash").map_err(|_| E::Unavailable)?;
        let authority: String = row.try_get("authority").map_err(|_| E::Unavailable)?;
        let ttl: i64 = row.try_get("device_ttl").map_err(|_| E::Unavailable)?;
        let template: KeyRow = sqlx::query_as("SELECT * FROM virtual_keys WHERE key_hash=?")
            .bind(&parent)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| E::Unavailable)?
            .ok_or(E::Invalid)?;
        let unchanged = snapshot(&template)? == authority;
        let template = template.to_virtual_key().map_err(|_| E::Unavailable)?;
        let now = Utc::now();
        if !eligible(&template, now) || !unchanged || !(1..=31_536_000).contains(&ttl) {
            return Err(E::Invalid);
        }
        let approved_expiry = now + chrono::Duration::seconds(ttl);
        let expires_at = template
            .expires_at
            .map_or(approved_expiry, |t| t.min(approved_expiry));
        let raw = generate_key();
        let hash = hash_key(&raw);
        let id = format!("device-{}", uuid::Uuid::new_v4().simple());
        let key = VirtualKey {
            key_id: id.clone(),
            key_hash: hash.clone(),
            name: Some("Enrolled device".into()),
            created_at: now,
            expires_at: Some(expires_at),
            valid_after: None,
            disabled: false,
            permissions: template.permissions,
            usage: KeyUsage::default(),
            metadata: serde_json::json!({"kind":"device_enrollment","template_id":template.key_id}),
            owner: template.owner,
            oauth_user: template.oauth_user,
            oauth_account: template.oauth_account,
        };
        sqlx::query("INSERT INTO virtual_keys(key_hash,key_id,name,created_at,expires_at,valid_after,disabled,permissions,usage,metadata,owner,oauth_user,oauth_account) VALUES(?,?,?,?,?,NULL,0,?,?,?,?,?,?)")
            .bind(&hash).bind(&id).bind(&key.name).bind(now.to_rfc3339()).bind(expires_at.to_rfc3339())
            .bind(serde_json::to_string(&key.permissions).map_err(|_| E::Unavailable)?)
            .bind(serde_json::to_string(&key.usage).map_err(|_| E::Unavailable)?)
            .bind(serde_json::to_string(&key.metadata).map_err(|_| E::Unavailable)?)
            .bind(&key.owner).bind(&key.oauth_user).bind(&key.oauth_account)
            .execute(&mut *tx).await.map_err(|_| E::Unavailable)?;
        tx.commit().await.map_err(|_| E::Unavailable)?;
        // No await after commit: synchronous publication to both auth indexes
        // happens before the secret can escape to the caller.
        self.human_id_index.insert(id.clone(), hash.clone());
        self.cache.insert(hash, key);
        Ok(DeviceCredential {
            key: raw,
            key_id: id,
            expires_at,
        })
    }
}

#[cfg(test)]
mod scope_tests;
#[cfg(test)]
mod tests;
