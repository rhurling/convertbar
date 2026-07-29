//! Server configuration, parsed from environment variables.
//!
//! `from_vars` takes an injected map so tests never mutate real process env (env
//! mutation is process-global and races across parallel `#[test]` threads).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use ipnet::IpNet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    Token(String),
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingAuth,
    WeakToken,
    BadTrustedProxy(String),
    BadBind(String),
}

/// Minimum viable auth token: long enough that online guessing is hopeless, and
/// not one character repeated. A floor against pathological input, NOT an entropy
/// estimator — `1234567890123456` passes, deliberately. Real entropy estimation
/// carries false-positive risk against legitimately random tokens.
pub fn token_is_strong(token: &str) -> bool {
    token.chars().count() >= 16
        && token
            .chars()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 8
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub auth: AuthMode,
    pub allowed_hosts: Vec<String>,
    pub browse_roots: Vec<PathBuf>,
    /// Addresses whose `X-Forwarded-For` is believed when resolving a request's
    /// throttling identity. Empty (the default) means the header is never read.
    pub trusted_proxies: Vec<IpNet>,
}

impl ServerConfig {
    /// `vars`: injected map for testability; `from_env()` wraps `std::env::vars()`.
    pub fn from_vars(vars: &HashMap<String, String>) -> Result<Self, ConfigError> {
        // The token is checked BEFORE the NO_AUTH fallthrough, so a weak token
        // plus NO_AUTH=1 is a startup failure rather than a silent downgrade to
        // open mode. Contradictory auth config should be surfaced, not guessed at.
        let auth = match vars.get("CONVERTBAR_AUTH_TOKEN") {
            Some(token) if !token.is_empty() => {
                if !token_is_strong(token) {
                    return Err(ConfigError::WeakToken);
                }
                AuthMode::Token(token.clone())
            }
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
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(str::to_string).collect())
            .unwrap_or_default();

        let browse_roots = vars
            .get("CONVERTBAR_BROWSE_ROOTS")
            .filter(|s| !s.is_empty())
            .map(|s| s.split(':').map(PathBuf::from).collect::<Vec<_>>())
            .filter(|roots| !roots.is_empty())
            .unwrap_or_else(|| vec![PathBuf::from("/")]);

        // A bare address means "exactly this host", i.e. a full-length prefix.
        // An unparsable entry is a hard error: skipping it would collapse every
        // client into one throttling bucket, invisibly.
        let mut trusted_proxies = Vec::new();
        if let Some(raw) = vars
            .get("CONVERTBAR_TRUSTED_PROXIES")
            .filter(|s| !s.is_empty())
        {
            for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let net = entry
                    .parse::<IpNet>()
                    .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                    .map_err(|_| ConfigError::BadTrustedProxy(entry.to_string()))?;
                trusted_proxies.push(net);
            }
        }

        Ok(Self {
            bind,
            auth,
            allowed_hosts,
            browse_roots,
            trusted_proxies,
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
        let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop")]))
            .unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(cfg.auth, AuthMode::Token("abcdefghijklmnop".to_string()));
        assert_eq!(cfg.allowed_hosts, Vec::<String>::new());
        assert_eq!(cfg.browse_roots, vec![PathBuf::from("/")]);
    }

    #[test]
    fn token_at_the_floor_is_accepted() {
        // Exactly 16 chars, 16 distinct — the boundary must pass, not just clear it.
        let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop")]))
            .unwrap();
        assert_eq!(cfg.auth, AuthMode::Token("abcdefghijklmnop".to_string()));
    }

    #[test]
    fn token_one_char_below_the_floor_is_rejected() {
        let err = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmno")]))
            .unwrap_err();
        assert_eq!(err, ConfigError::WeakToken);
    }

    #[test]
    fn long_token_with_too_few_distinct_chars_is_rejected() {
        // 32 chars but only 2 distinct: length alone must not be sufficient.
        let err = ServerConfig::from_vars(&vars(&[(
            "CONVERTBAR_AUTH_TOKEN",
            "abababababababababababababababab",
        )]))
        .unwrap_err();
        assert_eq!(err, ConfigError::WeakToken);
    }

    #[test]
    fn exactly_eight_distinct_chars_is_accepted_and_seven_is_not() {
        // 16 chars, 8 distinct — the distinct boundary, from the other side.
        assert!(
            ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghabcdefgh")]))
                .is_ok()
        );
        // 16 chars, 7 distinct.
        assert_eq!(
            ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefgabcdefgaa")]))
                .unwrap_err(),
            ConfigError::WeakToken
        );
    }

    #[test]
    fn token_length_counts_characters_not_bytes() {
        // 16 distinct multi-byte chars: 48 bytes, 16 chars.
        assert!(ServerConfig::from_vars(&vars(&[(
            "CONVERTBAR_AUTH_TOKEN",
            "日本語表示試験用文字列拡張確認済"
        )]))
        .is_ok());
        // 6 chars / 18 bytes — over a byte-based floor, under the char floor.
        // This is the case that pins char-counting; the accept above would pass
        // either way.
        assert_eq!(
            ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "日本語表示試")]))
                .unwrap_err(),
            ConfigError::WeakToken
        );
    }

    #[test]
    fn weak_token_is_rejected_even_when_no_auth_is_also_set() {
        // from_vars checks the token first, so a weak token does NOT silently
        // fall through to open mode. Contradictory auth config must be surfaced.
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "weak"),
            ("CONVERTBAR_NO_AUTH", "1"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::WeakToken);
    }

    #[test]
    fn trusted_proxies_parse_cidr_and_bare_addresses() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            (
                "CONVERTBAR_TRUSTED_PROXIES",
                "172.18.0.5,10.0.0.0/8,2001:db8::/32",
            ),
        ]))
        .unwrap();
        assert_eq!(
            cfg.trusted_proxies,
            vec![
                "172.18.0.5/32".parse::<IpNet>().unwrap(),
                "10.0.0.0/8".parse::<IpNet>().unwrap(),
                "2001:db8::/32".parse::<IpNet>().unwrap(),
            ]
        );
    }

    #[test]
    fn unparsable_trusted_proxy_is_a_hard_error_not_a_skipped_entry() {
        // Silently dropping an entry would collapse every client into one
        // throttling bucket — exactly the failure this variable exists to
        // prevent, and it would fail invisibly.
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_TRUSTED_PROXIES", "172.18.0.5,not-an-ip"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::BadTrustedProxy("not-an-ip".to_string()));
    }

    #[test]
    fn trusted_proxies_defaults_to_empty() {
        let cfg = ServerConfig::from_vars(&vars(&[("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop")]))
            .unwrap();
        assert!(cfg.trusted_proxies.is_empty());
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
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_BIND", "127.0.0.1"),
            ("CONVERTBAR_PORT", "9090"),
        ]))
        .unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:9090".parse().unwrap());
    }

    #[test]
    fn bad_bind_host_is_rejected() {
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_BIND", "not-an-ip"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::BadBind("not-an-ip".to_string()));
    }

    #[test]
    fn bad_port_is_rejected() {
        let err = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_PORT", "notaport"),
        ]))
        .unwrap_err();
        assert_eq!(err, ConfigError::BadBind("notaport".to_string()));
    }

    #[test]
    fn browse_roots_split_on_colon() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_BROWSE_ROOTS", "/data:/media"),
        ]))
        .unwrap();
        assert_eq!(
            cfg.browse_roots,
            vec![PathBuf::from("/data"), PathBuf::from("/media")]
        );
    }

    #[test]
    fn empty_browse_roots_falls_back_to_default() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_BROWSE_ROOTS", ""),
        ]))
        .unwrap();
        assert_eq!(cfg.browse_roots, vec![PathBuf::from("/")]);
    }

    #[test]
    fn empty_allowed_hosts_falls_back_to_default() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
            ("CONVERTBAR_ALLOWED_HOSTS", ""),
        ]))
        .unwrap();
        assert_eq!(cfg.allowed_hosts, Vec::<String>::new());
    }

    #[test]
    fn allowed_hosts_split_on_comma_only() {
        let cfg = ServerConfig::from_vars(&vars(&[
            ("CONVERTBAR_AUTH_TOKEN", "abcdefghijklmnop"),
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
