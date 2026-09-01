use std::{env, fs, time::Duration};

use anyhow::Context;
use chrono::Utc;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tracing::warn;
use uuid::Uuid;

use crate::{auth, config::Config};

pub async fn connect(config: &Config) -> anyhow::Result<SqlitePool> {
    fs::create_dir_all(&config.data_dir).with_context(|| {
        format!(
            "cannot create CarobaGuard data directory {}",
            config.data_dir.display()
        )
    })?;

    let options = config
        .database_url
        .parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;

    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

pub async fn bootstrap_admin(pool: &SqlitePool) -> anyhow::Result<()> {
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?;
    if user_count > 0 {
        return Ok(());
    }

    let username = env::var("CAROBAGUARD_ADMIN_USERNAME").unwrap_or_else(|_| "admin".into());
    let (password, generated) = match env::var("CAROBAGUARD_ADMIN_PASSWORD") {
        Ok(password) => (password, false),
        Err(_) => (auth::random_token(18), true),
    };
    if password.len() < 12 {
        anyhow::bail!("CAROBAGUARD_ADMIN_PASSWORD must have at least 12 characters");
    }

    let password_hash = auth::hash_password(password.clone()).await?;
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO users(id, username, password_hash, role, created_at, updated_at) \
         VALUES(?, ?, ?, 'admin', ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&username)
    .bind(password_hash)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    if generated {
        warn!(
            username,
            password, "generated initial administrator credentials; store the password now"
        );
    } else {
        warn!(username, "created initial administrator from environment");
    }
    Ok(())
}

pub async fn setting(pool: &SqlitePool, key: &str) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO settings(key, value, updated_at) VALUES(?, ?, unixepoch()) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_required_tables() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::test(temp.path().to_path_buf());
        let pool = connect(&config).await.unwrap();
        let names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(names.contains(&"users".to_owned()));
        assert!(names.contains(&"telemetry_samples".to_owned()));
        assert!(names.contains(&"audit_events".to_owned()));
        assert!(names.contains(&"ai_permissions".to_owned()));
    }
}
