//! Runtime configuration from environment variables.
//!
//! Everything is read and validated once at startup. A bad value stops the process
//! with a clear message instead of starting a server that half works: systemd
//! keeps restarting it and `journalctl -u lss` shows exactly which variable is wrong.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

/// Validated settings for one server run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Interface to bind. The spec requires 0.0.0.0 in production.
    pub host: IpAddr,
    /// TCP port. The spec requires 8080.
    pub port: u16,
    /// Number of actix worker threads (each one runs its own event loop).
    pub workers: usize,
    /// SQLite database file. Its parent directory is created if missing.
    pub database_path: PathBuf,
    /// A `tracing` filter such as `info` or `lss_server=debug,warn`.
    pub log_level: String,
}

/// Which variable was wrong, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub var: &'static str,
    pub value: String,
    pub reason: &'static str,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}={:?}: {}", self.var, self.value, self.reason)
    }
}

impl std::error::Error for ConfigError {}

pub const DEFAULT_HOST: &str = "0.0.0.0";
pub const DEFAULT_PORT: u16 = 8080;
/// Two workers = one per hyperthread on the m7i.large. More would only add
/// context switches on a single physical core.
pub const DEFAULT_WORKERS: usize = 2;
pub const DEFAULT_DATABASE_PATH: &str = "./data/lss.db";
pub const DEFAULT_LOG_LEVEL: &str = "info";

/// Upper bound on WORKERS. Far above anything useful on 2 vCPU; it only exists to
/// catch typos like WORKERS=200.
const MAX_WORKERS: usize = 64;

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds a config from any key lookup. Separate from [`Config::from_env`] so
    /// tests don't have to mutate the (process-global) environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        // Treat an empty variable like an unset one: `Environment=PORT=` in a unit
        // file should mean "default", not "invalid".
        let get = |name: &str| lookup(name).filter(|v| !v.trim().is_empty());

        let host = match get("HOST") {
            None => IpAddr::from([0, 0, 0, 0]),
            Some(v) => v.trim().parse().map_err(|_| ConfigError {
                var: "HOST",
                value: v.clone(),
                reason: "must be an IP address such as 0.0.0.0",
            })?,
        };

        let port = match get("PORT") {
            None => DEFAULT_PORT,
            Some(v) => match v.trim().parse::<u16>() {
                Ok(p) if p != 0 => p,
                _ => {
                    return Err(ConfigError {
                        var: "PORT",
                        value: v,
                        reason: "must be an integer from 1 to 65535",
                    });
                }
            },
        };

        let workers = match get("WORKERS") {
            None => DEFAULT_WORKERS,
            Some(v) => match v.trim().parse::<usize>() {
                Ok(n) if (1..=MAX_WORKERS).contains(&n) => n,
                _ => {
                    return Err(ConfigError {
                        var: "WORKERS",
                        value: v,
                        reason: "must be an integer from 1 to 64",
                    });
                }
            },
        };

        let database_path =
            PathBuf::from(get("DATABASE_PATH").unwrap_or_else(|| DEFAULT_DATABASE_PATH.to_owned()));

        let log_level = get("LOG_LEVEL").unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_owned());
        // Parse the filter now so a typo fails at startup, not silently later.
        if tracing_subscriber::EnvFilter::try_new(&log_level).is_err() {
            return Err(ConfigError {
                var: "LOG_LEVEL",
                value: log_level,
                reason: "must be a tracing filter such as info, warn or lss_server=debug",
            });
        }

        Ok(Self {
            host,
            port,
            workers,
            database_path,
            log_level,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let c = config(&[]).unwrap();
        assert_eq!(c.host.to_string(), DEFAULT_HOST);
        assert_eq!(c.port, 8080);
        assert_eq!(c.workers, 2);
        assert_eq!(c.database_path, PathBuf::from("./data/lss.db"));
        assert_eq!(c.log_level, "info");
    }

    #[test]
    fn explicit_values() {
        let c = config(&[
            ("HOST", "127.0.0.1"),
            ("PORT", "9000"),
            ("WORKERS", "4"),
            ("DATABASE_PATH", "/var/lib/lss/lss.db"),
            ("LOG_LEVEL", "warn"),
        ])
        .unwrap();
        assert_eq!(c.host.to_string(), "127.0.0.1");
        assert_eq!(c.port, 9000);
        assert_eq!(c.workers, 4);
        assert_eq!(c.database_path, PathBuf::from("/var/lib/lss/lss.db"));
        assert_eq!(c.log_level, "warn");
    }

    #[test]
    fn empty_means_default() {
        let c = config(&[("PORT", ""), ("WORKERS", " ")]).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.workers, 2);
    }

    #[test]
    fn rejects_bad_values() {
        assert_eq!(config(&[("HOST", "localhost")]).unwrap_err().var, "HOST");
        assert_eq!(config(&[("PORT", "0")]).unwrap_err().var, "PORT");
        assert_eq!(config(&[("PORT", "70000")]).unwrap_err().var, "PORT");
        assert_eq!(config(&[("PORT", "80a")]).unwrap_err().var, "PORT");
        assert_eq!(config(&[("WORKERS", "0")]).unwrap_err().var, "WORKERS");
        assert_eq!(config(&[("WORKERS", "200")]).unwrap_err().var, "WORKERS");
        assert_eq!(config(&[("WORKERS", "-1")]).unwrap_err().var, "WORKERS");
        assert_eq!(
            config(&[("LOG_LEVEL", "lss_server=loud")]).unwrap_err().var,
            "LOG_LEVEL"
        );
    }
}
