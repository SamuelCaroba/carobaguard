use std::{
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, bail};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub database_url: String,
    pub cookie_secure: bool,
    pub session_ttl_seconds: i64,
    pub terminal_max_sessions: usize,
    pub terminal_idle_timeout_seconds: u64,
    pub terminal_max_duration_seconds: u64,
    pub log_max_streams: usize,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = env::var("CAROBAGUARD_PORT")
            .unwrap_or_else(|_| "8090".to_owned())
            .parse::<u16>()
            .context("CAROBAGUARD_PORT must be a valid TCP port")?;
        let host = env::var("CAROBAGUARD_HOST")
            .unwrap_or_else(|_| "127.0.0.1".to_owned())
            .parse::<IpAddr>()
            .context("CAROBAGUARD_HOST must be an IP address")?;

        let data_dir = match env::var_os("CAROBAGUARD_DATA_DIR") {
            Some(value) => PathBuf::from(value),
            None => default_data_dir()?,
        };
        let database_url = format!("sqlite://{}", data_dir.join("carobaguard.db").display());

        let cookie_secure = parse_bool("CAROBAGUARD_COOKIE_SECURE", false)?;
        let session_ttl_seconds = env::var("CAROBAGUARD_SESSION_TTL_SECONDS")
            .unwrap_or_else(|_| "43200".to_owned())
            .parse::<i64>()
            .context("CAROBAGUARD_SESSION_TTL_SECONDS must be an integer")?;
        if session_ttl_seconds < 300 {
            bail!("CAROBAGUARD_SESSION_TTL_SECONDS must be at least 300");
        }
        let terminal_max_sessions = parse_integer("CAROBAGUARD_TERMINAL_MAX_SESSIONS", 4)?;
        if !(1..=32).contains(&terminal_max_sessions) {
            bail!("CAROBAGUARD_TERMINAL_MAX_SESSIONS must be between 1 and 32");
        }
        let terminal_idle_timeout_seconds =
            parse_integer("CAROBAGUARD_TERMINAL_IDLE_TIMEOUT_SECONDS", 900)?;
        if terminal_idle_timeout_seconds < 60 {
            bail!("CAROBAGUARD_TERMINAL_IDLE_TIMEOUT_SECONDS must be at least 60");
        }
        let terminal_max_duration_seconds =
            parse_integer("CAROBAGUARD_TERMINAL_MAX_DURATION_SECONDS", 14_400)?;
        if terminal_max_duration_seconds < 300 {
            bail!("CAROBAGUARD_TERMINAL_MAX_DURATION_SECONDS must be at least 300");
        }
        let log_max_streams = parse_integer("CAROBAGUARD_LOG_MAX_STREAMS", 8)?;
        if !(1..=64).contains(&log_max_streams) {
            bail!("CAROBAGUARD_LOG_MAX_STREAMS must be between 1 and 64");
        }

        Ok(Self {
            bind: SocketAddr::new(host, port),
            data_dir,
            database_url,
            cookie_secure,
            session_ttl_seconds,
            terminal_max_sessions,
            terminal_idle_timeout_seconds,
            terminal_max_duration_seconds,
            log_max_streams,
        })
    }

    #[cfg(test)]
    pub fn test(data_dir: PathBuf) -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0),
            database_url: format!("sqlite://{}", data_dir.join("test.db").display()),
            data_dir,
            cookie_secure: false,
            session_ttl_seconds: 3600,
            terminal_max_sessions: 4,
            terminal_idle_timeout_seconds: 900,
            terminal_max_duration_seconds: 14_400,
            log_max_streams: 8,
        }
    }
}

fn default_data_dir() -> anyhow::Result<PathBuf> {
    if let Some(value) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(value).join("carobaguard"));
    }
    let home = env::var_os("HOME")
        .context("cannot determine data directory; set CAROBAGUARD_DATA_DIR explicitly")?;
    Ok(PathBuf::from(home).join(".local/share/carobaguard"))
}

fn parse_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be true or false"),
    }
}

fn parse_integer<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr + Copy,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env::var(name).map_or_else(
        |_| Ok(default),
        |value| {
            value
                .parse::<T>()
                .with_context(|| format!("{name} must be an integer"))
        },
    )
}
