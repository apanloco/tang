//! Who may talk to the server; see `docs/web-ui.md` §8.
//!
//! The server loads native code, and any web page can open a WebSocket to
//! `localhost`, so a connection is checked before anything is done for it:
//!
//! - **`Host` and `Origin`**, on every request, against what the server listens on.
//!   That stops cross-site WebSocket hijacking (a page on another origin driving the
//!   session) and DNS rebinding (a name of an attacker's that resolves to
//!   127.0.0.1, and so arrives with its own `Host`).
//! - **A token**, for a server that listens beyond loopback: unguessable, given to
//!   the person who started the server, and sent in the first message of every
//!   connection.
//!
//! Everything here is pure: the headers and the token are passed in, so that the
//! policy can be tested without a socket.

#![cfg_attr(not(test), expect(dead_code, reason = "the server (later in step 10) applies it"))]

use std::net::SocketAddr;

/// Why a request is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The `Host` header is missing or is not one this server answers to.
    Host,
    /// The `Origin` header names a page this server does not serve.
    Origin,
}

/// A secret a client has to present, and how to compare it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// 128 random bits, as 32 hex digits.
    pub fn generate() -> Token {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the operating system has no random numbers to give");
        Token(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn new(secret: impl Into<String>) -> Token {
        Token(secret.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `presented` is the token. It takes as long for any wrong guess of the
    /// right length, so the time says nothing about how many leading characters were
    /// right.
    pub fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        if a.is_empty() || a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |difference, (x, y)| difference | (x ^ y)) == 0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// What the server is willing to talk to.
#[derive(Clone, Debug)]
pub struct Policy {
    bind: SocketAddr,
    token: Option<Token>,
    /// Other pages that may open a connection, as `scheme://host:port` (a dev
    /// server on another port, for instance).
    extra_origins: Vec<String>,
}

impl Policy {
    /// The policy for a server listening on `bind`. Beyond loopback a token is
    /// required (anyone on the network can reach the port), and without one this is
    /// an error; on loopback a token is optional.
    pub fn new(bind: SocketAddr, token: Option<Token>, extra_origins: Vec<String>) -> Result<Policy, String> {
        if !bind.ip().is_loopback() && token.is_none() {
            return Err(format!("{bind} is reachable from the network: it needs a token"));
        }
        let extra_origins = extra_origins.into_iter().map(|o| o.trim_end_matches('/').to_ascii_lowercase()).collect();
        Ok(Policy { bind, token, extra_origins })
    }

    pub fn is_loopback(&self) -> bool {
        self.bind.ip().is_loopback()
    }

    /// Whether the first message of a connection has to be `Auth`.
    pub fn needs_token(&self) -> bool {
        self.token.is_some()
    }

    /// Whether `presented` is the token (and there is one to present).
    pub fn token_matches(&self, presented: &str) -> bool {
        self.token.as_ref().is_some_and(|token| token.matches(presented))
    }

    /// Check the `Host` and `Origin` headers of a request (`None` for a header that
    /// is absent).
    ///
    /// On loopback, `Host` has to be this machine by one of its names and the port
    /// the server listens on, and an `Origin`, if there is one, a page served by this
    /// very server (or an extra origin that was allowed). A request with no `Origin`
    /// does not come from a web page (a page always sends one), and is let through.
    /// Beyond loopback the host names cannot be listed, so `Host` is not restricted
    /// (the token is what keeps others out) and an `Origin` has to be the page of
    /// the `Host` it came to.
    pub fn check(&self, host: Option<&str>, origin: Option<&str>) -> Result<(), Refusal> {
        let host = host.and_then(Authority::parse).ok_or(Refusal::Host)?;
        if self.is_loopback() && !self.loopback_authority(&host) {
            return Err(Refusal::Host);
        }
        let Some(origin) = origin else { return Ok(()) };
        let origin = origin.trim();
        if self.extra_origins.iter().any(|o| o.eq_ignore_ascii_case(origin)) {
            return Ok(());
        }
        let (scheme, authority) = origin.split_once("://").ok_or(Refusal::Origin)?;
        let authority = Authority::parse(authority).ok_or(Refusal::Origin)?;
        let scheme_port = match scheme.to_ascii_lowercase().as_str() {
            "http" => 80,
            "https" if !self.is_loopback() => 443,
            _ => return Err(Refusal::Origin),
        };
        let same_page = if self.is_loopback() {
            // (An absent port is the scheme's, which is 80: the only scheme allowed here.)
            self.loopback_authority(&authority)
        } else {
            authority.host == host.host && authority.port.unwrap_or(scheme_port) == host.port.unwrap_or(scheme_port)
        };
        if same_page { Ok(()) } else { Err(Refusal::Origin) }
    }

    /// Whether `authority` is this machine's loopback by a name a browser uses, on
    /// the port the server listens on.
    fn loopback_authority(&self, authority: &Authority) -> bool {
        let names = ["localhost", "127.0.0.1", "[::1]"];
        names.contains(&authority.host.as_str()) && authority.port.unwrap_or(80) == self.bind.port()
    }
}

/// The host and port of a `Host` header or of an origin.
#[derive(Debug, PartialEq, Eq)]
struct Authority {
    /// Lower case; an IPv6 address keeps its brackets.
    host: String,
    port: Option<u16>,
}

impl Authority {
    /// `host`, `host:port`, `[v6]` or `[v6]:port`; nothing else (no user info, no
    /// path, no spaces).
    fn parse(text: &str) -> Option<Authority> {
        let text = text.trim();
        if text.is_empty() || text.contains(|c: char| c.is_whitespace() || matches!(c, '@' | '/' | '\\' | '?' | '#')) {
            return None;
        }
        let (host, port) = if text.starts_with('[') {
            let end = text.find(']')?;
            let (host, rest) = text.split_at(end + 1);
            match rest {
                "" => (host, None),
                _ => (host, Some(rest.strip_prefix(':')?)),
            }
        } else {
            match text.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (text, None),
            }
        };
        if host.is_empty() || host == "[]" {
            return None;
        }
        let port = match port {
            None => None,
            Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => Some(digits.parse().ok()?),
            Some(_) => return None,
        };
        Some(Authority { host: host.to_ascii_lowercase(), port })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback(port: u16) -> Policy {
        Policy::new(SocketAddr::from(([127, 0, 0, 1], port)), None, vec![]).unwrap()
    }

    fn lan() -> Policy {
        Policy::new("192.168.1.5:8080".parse().unwrap(), Some(Token::new("secret")), vec![]).unwrap()
    }

    #[test]
    fn a_server_that_the_network_can_reach_needs_a_token() {
        for addr in ["0.0.0.0:8080", "192.168.1.5:8080", "[::]:8080", "[fe80::1]:8080"] {
            assert!(Policy::new(addr.parse().unwrap(), None, vec![]).is_err(), "{addr}");
            assert!(Policy::new(addr.parse().unwrap(), Some(Token::new("t")), vec![]).is_ok(), "{addr}");
        }
        for addr in ["127.0.0.1:8080", "[::1]:8080", "127.1.2.3:8080"] {
            assert!(Policy::new(addr.parse().unwrap(), None, vec![]).is_ok(), "{addr}");
        }
    }

    #[test]
    fn only_a_server_with_a_token_asks_for_one() {
        assert!(!loopback(8080).needs_token());
        assert!(lan().needs_token());
        let guarded = Policy::new("127.0.0.1:8080".parse().unwrap(), Some(Token::new("t")), vec![]).unwrap();
        assert!(guarded.needs_token(), "a token is allowed on loopback too");
    }

    #[test]
    fn the_token_is_accepted_only_exactly() {
        let policy = lan();
        assert!(policy.token_matches("secret"));
        for wrong in ["", "secre", "secrets", "Secret", "secreT", " secret", "secret "] {
            assert!(!policy.token_matches(wrong), "{wrong:?}");
        }
        assert!(!loopback(8080).token_matches(""), "no token: nothing matches, not even nothing");
        assert!(!loopback(8080).token_matches("secret"));
        assert!(!Policy::new("127.0.0.1:1".parse().unwrap(), Some(Token::new("")), vec![]).unwrap().token_matches(""));
    }

    #[test]
    fn generated_tokens_are_32_hex_digits_and_differ() {
        let (a, b) = (Token::generate(), Token::generate());
        assert_eq!(a.as_str().len(), 32);
        assert!(a.as_str().bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(a, b);
        assert!(a.matches(a.as_str()));
        assert!(!a.matches(b.as_str()));
    }

    #[test]
    fn a_token_does_not_show_in_debug_output() {
        assert_eq!(format!("{:?}", Token::new("hunter2")), "Token(..)");
        let policy = lan();
        assert!(!format!("{policy:?}").contains("secret"));
    }

    // -- Loopback --

    #[test]
    fn on_loopback_the_host_is_this_machine_on_the_servers_port() {
        let policy = loopback(8080);
        for host in ["localhost:8080", "127.0.0.1:8080", "[::1]:8080", "LocalHost:8080", "LOCALHOST:8080"] {
            assert_eq!(policy.check(Some(host), None), Ok(()), "{host}");
        }
        for host in [
            "evil.example:8080",
            "0.0.0.0:8080",
            "[::]:8080",
            "localhost:9999",
            "localhost",
            "localhost:80",
            "127.0.0.1.evil.example:8080",
            "localhost.evil.example:8080",
            "evil.example",
            "localhost:8080@evil.example",
            "evil.example#localhost:8080",
            "localhost:8080/",
            "localhost: 8080",
            "localhost:80a80",
            "localhost:",
            ":8080",
            "[::1]x:8080",
            "[::1:8080",
            "",
            "   ",
        ] {
            assert_eq!(policy.check(Some(host), None), Err(Refusal::Host), "{host:?}");
        }
        assert_eq!(policy.check(None, None), Err(Refusal::Host), "a request with no Host");
    }

    #[test]
    fn on_port_80_the_host_may_leave_the_port_out() {
        let policy = loopback(80);
        assert_eq!(policy.check(Some("localhost"), None), Ok(()));
        assert_eq!(policy.check(Some("localhost:80"), None), Ok(()));
        assert_eq!(policy.check(Some("localhost:8080"), None), Err(Refusal::Host));
    }

    #[test]
    fn on_loopback_a_page_of_the_server_may_open_a_connection_and_no_other() {
        let policy = loopback(8080);
        for origin in ["http://localhost:8080", "http://127.0.0.1:8080", "http://[::1]:8080", "HTTP://LOCALHOST:8080"] {
            assert_eq!(policy.check(Some("localhost:8080"), Some(origin)), Ok(()), "{origin}");
        }
        for origin in [
            "http://evil.example",
            "http://evil.example:8080",
            "http://localhost:9999",
            "http://localhost",
            "https://localhost:8080",
            "null",
            "",
            "localhost:8080",
            "http://localhost:8080/",
            "http://localhost:8080/page",
            "http://user@localhost:8080",
            "ftp://localhost:8080",
            "http://localhost:8080.evil.example",
            "http://evil.example/localhost:8080",
        ] {
            assert_eq!(policy.check(Some("localhost:8080"), Some(origin)), Err(Refusal::Origin), "{origin:?}");
        }
    }

    #[test]
    fn a_request_with_no_origin_is_not_from_a_web_page_and_passes_the_origin_check() {
        assert_eq!(loopback(8080).check(Some("127.0.0.1:8080"), None), Ok(()));
        assert_eq!(lan().check(Some("192.168.1.5:8080"), None), Ok(()));
    }

    #[test]
    fn an_origin_that_was_allowed_by_name_is_let_in() {
        let policy = Policy::new(
            "127.0.0.1:8080".parse().unwrap(),
            None,
            vec!["http://localhost:5173/".to_string(), "HTTP://Dev.Example:3000".to_string()],
        )
        .unwrap();
        assert_eq!(policy.check(Some("localhost:8080"), Some("http://localhost:5173")), Ok(()));
        assert_eq!(policy.check(Some("localhost:8080"), Some("http://dev.example:3000")), Ok(()));
        assert_eq!(policy.check(Some("localhost:8080"), Some("http://localhost:5174")), Err(Refusal::Origin));
        assert_eq!(policy.check(Some("localhost:8080"), Some("http://dev.example:3001")), Err(Refusal::Origin));
        // An allowed origin does not make an unknown Host acceptable.
        assert_eq!(policy.check(Some("evil.example:8080"), Some("http://localhost:5173")), Err(Refusal::Host));
    }

    // -- Beyond loopback --

    #[test]
    fn beyond_loopback_the_host_is_not_restricted_but_has_to_be_there_and_well_formed() {
        let policy = lan();
        for host in ["192.168.1.5:8080", "mymachine.local:8080", "mymachine.local", "[fe80::1]:8080"] {
            assert_eq!(policy.check(Some(host), None), Ok(()), "{host}");
        }
        assert_eq!(policy.check(None, None), Err(Refusal::Host));
        assert_eq!(policy.check(Some("a b"), None), Err(Refusal::Host));
        assert_eq!(policy.check(Some("a@b"), None), Err(Refusal::Host));
    }

    #[test]
    fn beyond_loopback_an_origin_has_to_be_the_page_of_the_host_it_came_to() {
        let policy = lan();
        assert_eq!(policy.check(Some("192.168.1.5:8080"), Some("http://192.168.1.5:8080")), Ok(()));
        assert_eq!(policy.check(Some("mymachine.local:8080"), Some("http://MYMACHINE.local:8080")), Ok(()));
        assert_eq!(policy.check(Some("mymachine.local:8080"), Some("https://mymachine.local:8080")), Ok(()), "behind a TLS proxy");
        assert_eq!(policy.check(Some("mymachine.local"), Some("http://mymachine.local:80")), Ok(()), "default ports agree");
        for origin in ["http://evil.example", "http://evil.example:8080", "http://192.168.1.5:9090", "null", "http://192.168.1.5:8080/x"] {
            assert_eq!(policy.check(Some("192.168.1.5:8080"), Some(origin)), Err(Refusal::Origin), "{origin}");
        }
    }

    #[test]
    fn authorities_are_parsed_strictly() {
        let host = |text| Authority::parse(text).map(|a| (a.host, a.port));
        assert_eq!(host("example.com"), Some(("example.com".to_string(), None)));
        assert_eq!(host("Example.COM:8080"), Some(("example.com".to_string(), Some(8080))));
        assert_eq!(host("[::1]"), Some(("[::1]".to_string(), None)));
        assert_eq!(host("[::1]:80"), Some(("[::1]".to_string(), Some(80))));
        assert_eq!(host("  localhost:1  "), Some(("localhost".to_string(), Some(1))));
        for bad in ["", "[]", "[]:80", "a:b", "a:", "a:99999", "a:-1", "a:1:2", "::1", "[::1", "[::1]80", "a/b", "a@b", "a b", "a?b", "a#b", "a\\b"] {
            assert_eq!(host(bad), None, "{bad:?}");
        }
    }
}
