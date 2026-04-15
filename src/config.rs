//! Environment-variable-based configuration.

use std::ffi::OsString;

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: String,
    pub pow_difficulty: u32,
    pub nonce_expiration_seconds: i64,
    pub submit_callback: Option<String>,
    pub bind_addr: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid(&'static str, String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Missing(name) => write!(f, "missing env var: {name}"),
            ConfigError::Invalid(name, v) => write!(f, "invalid value for {name}: {v}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Source of environment-ish lookups; abstracted so we can test without
/// mutating the process env.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct SystemEnv;

impl EnvSource for SystemEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var_os(key).and_then(|v: OsString| v.into_string().ok())
    }
}

impl<F: Fn(&str) -> Option<String>> EnvSource for F {
    fn get(&self, key: &str) -> Option<String> {
        (self)(key)
    }
}

pub fn load<E: EnvSource>(env: &E) -> Result<Config, ConfigError> {
    let db_path = env
        .get("SLOOS_DB_PATH")
        .ok_or(ConfigError::Missing("SLOOS_DB_PATH"))?;
    let pow_difficulty_s = env
        .get("SLOOS_POW_DIFFICULTY")
        .ok_or(ConfigError::Missing("SLOOS_POW_DIFFICULTY"))?;
    let pow_difficulty: u32 = pow_difficulty_s
        .parse()
        .map_err(|_| ConfigError::Invalid("SLOOS_POW_DIFFICULTY", pow_difficulty_s))?;
    let nonce_exp_s = env
        .get("SLOOS_NONCE_EXPIRATION_SECONDS")
        .ok_or(ConfigError::Missing("SLOOS_NONCE_EXPIRATION_SECONDS"))?;
    let nonce_expiration_seconds: i64 = nonce_exp_s
        .parse()
        .map_err(|_| ConfigError::Invalid("SLOOS_NONCE_EXPIRATION_SECONDS", nonce_exp_s))?;
    if nonce_expiration_seconds <= 0 {
        return Err(ConfigError::Invalid(
            "SLOOS_NONCE_EXPIRATION_SECONDS",
            nonce_expiration_seconds.to_string(),
        ));
    }
    let submit_callback = env.get("SLOOS_SUBMIT_CALLBACK").filter(|s| !s.is_empty());
    let bind_addr = env
        .get("SLOOS_BIND_ADDR")
        .unwrap_or_else(|| "127.0.0.1:3000".to_string());

    Ok(Config {
        db_path,
        pow_difficulty,
        nonce_expiration_seconds,
        submit_callback,
        bind_addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MapEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|s| s.to_string())
        }
    }

    #[test]
    fn load_ok_with_all_vars() {
        let env = MapEnv(HashMap::from([
            ("SLOOS_DB_PATH", "/tmp/x.db"),
            ("SLOOS_POW_DIFFICULTY", "10"),
            ("SLOOS_NONCE_EXPIRATION_SECONDS", "300"),
            ("SLOOS_SUBMIT_CALLBACK", "echo hi"),
        ]));
        let cfg = load(&env).unwrap();
        assert_eq!(cfg.db_path, "/tmp/x.db");
        assert_eq!(cfg.pow_difficulty, 10);
        assert_eq!(cfg.nonce_expiration_seconds, 300);
        assert_eq!(cfg.submit_callback.as_deref(), Some("echo hi"));
    }

    #[test]
    fn load_optional_callback_absent() {
        let env = MapEnv(HashMap::from([
            ("SLOOS_DB_PATH", "/tmp/x.db"),
            ("SLOOS_POW_DIFFICULTY", "1"),
            ("SLOOS_NONCE_EXPIRATION_SECONDS", "60"),
        ]));
        let cfg = load(&env).unwrap();
        assert!(cfg.submit_callback.is_none());
    }

    #[test]
    fn load_missing_required() {
        let env = MapEnv(HashMap::from([("SLOOS_POW_DIFFICULTY", "1")]));
        let err = load(&env).unwrap_err();
        assert!(matches!(err, ConfigError::Missing("SLOOS_DB_PATH")));
    }

    #[test]
    fn load_invalid_difficulty() {
        let env = MapEnv(HashMap::from([
            ("SLOOS_DB_PATH", "/tmp/x.db"),
            ("SLOOS_POW_DIFFICULTY", "not-a-number"),
            ("SLOOS_NONCE_EXPIRATION_SECONDS", "60"),
        ]));
        let err = load(&env).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid("SLOOS_POW_DIFFICULTY", _)
        ));
    }

    #[test]
    fn load_rejects_non_positive_expiration() {
        let env = MapEnv(HashMap::from([
            ("SLOOS_DB_PATH", "/tmp/x.db"),
            ("SLOOS_POW_DIFFICULTY", "1"),
            ("SLOOS_NONCE_EXPIRATION_SECONDS", "0"),
        ]));
        let err = load(&env).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid("SLOOS_NONCE_EXPIRATION_SECONDS", _)
        ));
    }
}
