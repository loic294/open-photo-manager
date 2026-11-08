//! Runtime configuration, read once at startup from `APM_*` environment
//! variables.
//!
//! Parsing goes through [`Config::from_lookup`] so it can be unit-tested without
//! touching the process environment.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::security::host::parse_allowed_hosts;
const DEFAULT_VOLUMES_DIR: &str = "/volumes";
const DEFAULT_DATA_DIR: &str = "/data";
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
const DEFAULT_LOG: &str = "info";
const DEFAULT_WEB_DIR: &str = "./web/dist";
const DEFAULT_IMMICH_QUIET_SECS: u64 = 15;
const MAX_IMMICH_QUIET_SECS: u64 = 600;

/// An environment variable holds a value the server cannot use.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid {var}={value:?}: {reason}")]
pub struct ConfigError {
    pub var: &'static str,
    pub value: String,
    pub reason: String,
}

impl ConfigError {
    fn new(var: &'static str, value: &str, reason: impl std::fmt::Display) -> Self {
        Self {
            var,
            value: value.to_owned(),
            reason: reason.to_string(),
        }
    }
}

/// Server configuration. Empty environment variables count as unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `APM_VOLUMES_DIR` (default `/volumes`): every immediate subdirectory is a
    /// volume, typically one bind-mounted NAS share each.
    pub volumes_dir: PathBuf,
    /// `APM_DATA_DIR` (default `/data`): writable state (SQLite index, caches,
    /// encrypted secrets, journals).
    pub data_dir: PathBuf,
    /// `APM_LISTEN` (default `0.0.0.0:8080`).
    pub listen: SocketAddr,
    /// `APM_ALLOWED_HOSTS`: extra `Host` header values the server answers to,
    /// separated by commas and/or spaces, validated and lowercased: `host`,
    /// `host:port` or `*.domain` (see `security::host`). `localhost` and IP
    /// literals are always allowed.
    pub allowed_hosts: Vec<String>,
    /// `APM_IMMICH_QUIET_SECS` (default 15, 1 to 600): how long file operations must have been
    /// quiet before the changes they queued are mirrored into Immich (see `immich::outbox`).
    pub immich_quiet_secs: u64,
    /// `APM_INDEX_CONCURRENCY` (default `min(2, cores)`, at least 1): files the
    /// indexer processes at once.
    pub index_concurrency: usize,
    /// `APM_LOG` (default `info`): `tracing` filter directives such as
    /// `info,apm_server::indexer=debug`.
    pub log: String,
    /// `APM_WEB_DIR` (default `./web/dist`): built web app. The Docker image
    /// sets `/app/web`.
    pub web_dir: PathBuf,
}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let cores = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self::from_lookup(|key| std::env::var(key).ok(), cores)
    }

    /// Builds a configuration from an arbitrary variable lookup; `cores` is the
    /// available parallelism used for the indexer default.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        cores: usize,
    ) -> Result<Self, ConfigError> {
        let var = |name: &str| {
            get(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        let listen_raw = var("APM_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen = listen_raw
            .parse::<SocketAddr>()
            .map_err(|err| ConfigError::new("APM_LISTEN", &listen_raw, err))?;

        let index_concurrency = match var("APM_INDEX_CONCURRENCY") {
            None => cores.clamp(1, 2),
            Some(raw) => match raw.parse::<usize>() {
                Ok(0) => {
                    return Err(ConfigError::new(
                        "APM_INDEX_CONCURRENCY",
                        &raw,
                        "must be at least 1",
                    ));
                }
                Ok(count) => count,
                Err(err) => return Err(ConfigError::new("APM_INDEX_CONCURRENCY", &raw, err)),
            },
        };

        let allowed_hosts = match var("APM_ALLOWED_HOSTS") {
            None => Vec::new(),
            Some(raw) => parse_allowed_hosts(&raw)
                .map_err(|reason| ConfigError::new("APM_ALLOWED_HOSTS", &raw, reason))?,
        };

        let immich_quiet_secs = match var("APM_IMMICH_QUIET_SECS") {
            None => DEFAULT_IMMICH_QUIET_SECS,
            Some(raw) => match raw.parse::<u64>() {
                Ok(secs) if (1..=MAX_IMMICH_QUIET_SECS).contains(&secs) => secs,
                Ok(_) => {
                    return Err(ConfigError::new(
                        "APM_IMMICH_QUIET_SECS",
                        &raw,
                        format!("must be between 1 and {MAX_IMMICH_QUIET_SECS}"),
                    ));
                }
                Err(err) => return Err(ConfigError::new("APM_IMMICH_QUIET_SECS", &raw, err)),
            },
        };

        Ok(Self {
            volumes_dir: var("APM_VOLUMES_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_VOLUMES_DIR), PathBuf::from),
            data_dir: var("APM_DATA_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from),
            listen,
            allowed_hosts,
            immich_quiet_secs,
            index_concurrency,
            log: var("APM_LOG").unwrap_or_else(|| DEFAULT_LOG.to_owned()),
            web_dir: var("APM_WEB_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_WEB_DIR), PathBuf::from),
        })
    }

    /// Configuration rooted in `dir` for tests: `volumes/`, `data/` and `web/`
    /// below it, an ephemeral loopback port, `localhost` as the allowed host, a
    /// single indexer worker and a one second quiet window for the Immich outbox.
    /// Does not create any directory.
    pub fn for_tests(dir: &Path) -> Self {
        Self {
            volumes_dir: dir.join("volumes"),
            data_dir: dir.join("data"),
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            allowed_hosts: vec!["localhost".to_owned()],
            immich_quiet_secs: 1,
            index_concurrency: 1,
            log: "warn".to_owned(),
            web_dir: dir.join("web"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| vars.get(key).cloned()
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        let config = Config::from_lookup(lookup(&[]), 8).unwrap();
        assert_eq!(config.volumes_dir, PathBuf::from("/volumes"));
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.listen, "0.0.0.0:8080".parse::<SocketAddr>().unwrap());
        assert!(config.allowed_hosts.is_empty());
        assert_eq!(config.immich_quiet_secs, 15);
        assert_eq!(config.index_concurrency, 2);
        assert_eq!(config.log, "info");
        assert_eq!(config.web_dir, PathBuf::from("./web/dist"));
    }

    #[test]
    fn index_concurrency_default_follows_core_count() {
        let single = Config::from_lookup(lookup(&[]), 1).unwrap();
        assert_eq!(single.index_concurrency, 1);
        let zero = Config::from_lookup(lookup(&[]), 0).unwrap();
        assert_eq!(zero.index_concurrency, 1);
        let many = Config::from_lookup(lookup(&[]), 64).unwrap();
        assert_eq!(many.index_concurrency, 2);
    }

    #[test]
    fn every_variable_can_be_overridden() {
        let config = Config::from_lookup(
            lookup(&[
                ("APM_VOLUMES_DIR", "/mnt/shares"),
                ("APM_DATA_DIR", "/var/lib/apm"),
                ("APM_LISTEN", "127.0.0.1:9000"),
                ("APM_ALLOWED_HOSTS", "nas.local"),
                ("APM_IMMICH_QUIET_SECS", "45"),
                ("APM_INDEX_CONCURRENCY", "6"),
                ("APM_LOG", "debug,sqlx=warn"),
                ("APM_WEB_DIR", "/app/web"),
            ]),
            8,
        )
        .unwrap();
        assert_eq!(config.volumes_dir, PathBuf::from("/mnt/shares"));
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/apm"));
        assert_eq!(
            config.listen,
            "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.allowed_hosts, vec!["nas.local".to_owned()]);
        assert_eq!(config.immich_quiet_secs, 45);
        assert_eq!(config.index_concurrency, 6);
        assert_eq!(config.log, "debug,sqlx=warn");
        assert_eq!(config.web_dir, PathBuf::from("/app/web"));
    }

    #[test]
    fn empty_and_blank_values_count_as_unset() {
        let config = Config::from_lookup(
            lookup(&[
                ("APM_LISTEN", ""),
                ("APM_LOG", "   "),
                ("APM_IMMICH_QUIET_SECS", ""),
                ("APM_INDEX_CONCURRENCY", ""),
                ("APM_ALLOWED_HOSTS", " "),
            ]),
            8,
        )
        .unwrap();
        assert_eq!(config.listen.port(), 8080);
        assert_eq!(config.log, "info");
        assert_eq!(config.immich_quiet_secs, 15);
        assert_eq!(config.index_concurrency, 2);
        assert!(config.allowed_hosts.is_empty());
    }

    #[test]
    fn allowed_hosts_are_trimmed_lowercased_and_filtered() {
        let config = Config::from_lookup(
            lookup(&[(
                "APM_ALLOWED_HOSTS",
                " NAS.local, photos.example.com:8080 ,,",
            )]),
            4,
        )
        .unwrap();
        assert_eq!(
            config.allowed_hosts,
            vec!["nas.local".to_owned(), "photos.example.com:8080".to_owned()]
        );
    }

    #[test]
    fn allowed_hosts_accept_whitespace_wildcards_and_ports() {
        let config = Config::from_lookup(
            lookup(&[(
                "APM_ALLOWED_HOSTS",
                "nas.local\tPhotos.Example.com:8080\n*.Example.org, [::1]:9000",
            )]),
            4,
        )
        .unwrap();
        assert_eq!(
            config.allowed_hosts,
            vec![
                "nas.local".to_owned(),
                "photos.example.com:8080".to_owned(),
                "*.example.org".to_owned(),
                "[::1]:9000".to_owned(),
            ]
        );
    }

    #[test]
    fn unusable_allowed_hosts_are_refused_at_startup() {
        for raw in [
            "*",
            "nas.local, *",
            "https://nas.local",
            "nas.local/photos",
            "user@nas.local",
            "nas.*",
            "nas.local:0",
            "nas.local:http",
            "nas..local",
            "*.",
            "*.192.168.1.1",
            "exa mple,\u{e9}.local",
        ] {
            let err = Config::from_lookup(lookup(&[("APM_ALLOWED_HOSTS", raw)]), 4).expect_err(raw);
            assert_eq!(err.var, "APM_ALLOWED_HOSTS", "{raw}");
            assert!(err.to_string().contains("APM_ALLOWED_HOSTS"), "{raw}");
        }
    }

    #[test]
    fn invalid_values_name_the_variable() {
        let listen = Config::from_lookup(lookup(&[("APM_LISTEN", "not-an-address")]), 4);
        assert_eq!(listen.unwrap_err().var, "APM_LISTEN");

        let text = Config::from_lookup(lookup(&[("APM_INDEX_CONCURRENCY", "many")]), 4);
        assert_eq!(text.unwrap_err().var, "APM_INDEX_CONCURRENCY");

        let zero = Config::from_lookup(lookup(&[("APM_INDEX_CONCURRENCY", "0")]), 4).unwrap_err();
        assert_eq!(zero.var, "APM_INDEX_CONCURRENCY");
        assert!(zero.to_string().contains("at least 1"));
    }

    #[test]
    fn the_immich_quiet_window_is_between_one_and_six_hundred_seconds() {
        for raw in ["1", "15", "600"] {
            let config = Config::from_lookup(lookup(&[("APM_IMMICH_QUIET_SECS", raw)]), 4).unwrap();
            assert_eq!(config.immich_quiet_secs, raw.parse::<u64>().unwrap());
        }
        for raw in ["0", "601", "-1", "soon", "1.5"] {
            let err =
                Config::from_lookup(lookup(&[("APM_IMMICH_QUIET_SECS", raw)]), 4).expect_err(raw);
            assert_eq!(err.var, "APM_IMMICH_QUIET_SECS", "{raw}");
        }
    }

    #[test]
    fn for_tests_is_rooted_in_the_given_directory() {
        let config = Config::for_tests(Path::new("/scratch/run"));
        assert_eq!(config.volumes_dir, PathBuf::from("/scratch/run/volumes"));
        assert_eq!(config.data_dir, PathBuf::from("/scratch/run/data"));
        assert_eq!(config.web_dir, PathBuf::from("/scratch/run/web"));
        assert_eq!(config.index_concurrency, 1);
        assert!(config.listen.ip().is_loopback());
    }
}
