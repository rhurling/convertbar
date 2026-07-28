//! Server configuration, parsed from environment variables.
//!
//! `from_vars` takes an injected map so tests never mutate real process env (env
//! mutation is process-global and races across parallel `#[test]` threads).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    Token(String),
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingAuth,
    BadBind(String),
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub auth: AuthMode,
    pub allowed_hosts: Vec<String>,
    pub browse_roots: Vec<PathBuf>,
}

impl ServerConfig {
    /// `vars`: injected map for testability; `from_env()` wraps `std::env::vars()`.
    pub fn from_vars(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let auth = match vars.get("CONVERTBAR_AUTH_TOKEN") {
            Some(token) if !token.is_empty() => AuthMode::Token(token.clone()),
            _ if vars.get("CONVERTBAR_NO_AUTH").map(String::as_str) == Some("1") => AuthMode::Open,
            _ => return Err(ConfigError::MissingAuth),
        };

        let host = vars
            .get("CONVERTBAR_BIND")
            .map(String::as_str)
            .unwrap_or("0.0.0.0");
        let ip: IpAddr = host
            .parse()
            .map_err(|_| ConfigError::BadBind(host.to_string()))?;

        let port_str = vars
            .get("CONVERTBAR_PORT")
            .map(String::as_str)
            .unwrap_or("8080");
        let port: u16 = port_str
            .parse()
            .map_err(|_| ConfigError::BadBind(port_str.to_string()))?;

        let bind = SocketAddr::new(ip, port);

        // Comma-separated ONLY: a colon split would mangle `host:port` entries and IPv6.
        let allowed_hosts = vars
            .get("CONVERTBAR_ALLOWED_HOSTS")
            .map(|s| s.split(',').map(str::to_string).collect())
            .unwrap_or_default();

        let browse_roots = vars
            .get("CONVERTBAR_BROWSE_ROOTS")
            .map(|s| s.split(':').map(PathBuf::from).collect::<Vec<_>>())
            .filter(|roots| !roots.is_empty())
            .unwrap_or_else(|| vec![PathBuf::from("/")]);

        Ok(Self {
            bind,
            auth,
            allowed_hosts,
            browse_roots,
        })
    }

    /// Thin, untested wrapper around `from_vars` for real process startup.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(&std::env::vars().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn defaults_when_only_token_set() {
        let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "secret")])).unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(cfg.auth, AuthMode::Token("secret".to_string()));
        assert_eq!(cfg.allowed_hosts, Vec::<String>::new());
        assert_eq!(cfg.browse_roots, vec![PathBuf::from("/")]);
    }

    #[test]
    fn missing_auth_without_token_or_no_auth_flag() {
        let err = ServerConfig::from_vars(&vars(&[])).unwrap_err();
        assert_eq!(err, ConfigError::MissingAuth);
    }

    #[test]
    fn no_auth_flag_enables_open_mode() {
        let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_NO_AUTH", "1")])).unwrap();
        assert_eq!(cfg.auth, AuthMode::Open);
    }

    #[test]
    fn custom_bind_and_port_are_parsed() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "t"),
            ("CONVERTBAR_BIND", "127.0.0.1"),
            ("CONVERTBAR_PORT", "9090"),
        ]))
        .unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:9090".parse().unwrap());
    }

    #[test]
    fn bad_bind_host_is_rejected() {
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "t"),
            ("CONVERTBAR_BIND", "not-an-ip"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::BadBind("not-an-ip".to_string()));
    }

    #[test]
    fn bad_port_is_rejected() {
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "t"),
            ("CONVERTBAR_PORT", "notaport"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::BadBind("notaport".to_string()));
    }

    #[test]
    fn browse_roots_split_on_colon() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "t"),
            ("CONVERTBAR_BROWSE_ROOTS", "/data:/media"),
        ]))
        .unwrap();
        assert_eq!(
            cfg.browse_roots,
            vec![PathBuf::from("/data"), PathBuf::from("/media")]
        );
    }

    #[test]
    fn allowed_hosts_split_on_comma_only() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "t"),
            ("CONVERTBAR_ALLOWED_HOSTS", "foo.local,bar.local:8080"),
        ]))
        .unwrap();
        // A colon inside a host:port entry must survive the split untouched.
        assert_eq!(
            cfg.allowed_hosts,
            vec!["foo.local".to_string(), "bar.local:8080".to_string()]
        );
    }
}
