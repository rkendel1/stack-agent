//! Host-provided configuration. Nothing here is read from the environment.

use std::time::Duration;

use ureq::tls::{PemItem, RootCerts, parse_pem};

use crate::secret::Secret;

/// Why a configuration was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Default connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline used when the request carries no attempt budget. A blocking read cannot be
/// interrupted, so this bounds how long an abandoned call can keep its worker alive.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(120);

/// Connection settings every provider shares.
#[derive(Clone, Debug)]
pub struct Common {
    base_url: String,
    pub(crate) key: Option<Secret>,
    pub(crate) extra_headers: Vec<(String, String)>,
    pub(crate) connect_timeout: Duration,
    pub(crate) default_deadline: Duration,
    /// Host-supplied trust roots that replace the bundled public roots. `None` keeps the bundle.
    pub(crate) root_certs: Option<RootCerts>,
    /// A host-configured HTTP CONNECT proxy. `None` connects directly. Never read from the
    /// environment.
    pub(crate) proxy: Option<ProxySettings>,
    /// A plain-`http` private-LAN endpoint accepted by the keyless opt-in. No header may be
    /// added to such a configuration, so nothing credential-like can ride along in the clear.
    insecure_lan: bool,
}

/// Headers the host may not set: the provider adapter owns them.
const RESERVED: [&str; 8] = [
    "authorization",
    "x-api-key",
    "content-type",
    "content-length",
    "host",
    "accept",
    "anthropic-version",
    "transfer-encoding",
];

/// The host part of `host[:port][/path]`, bracketed IPv6 included.
fn host_of(rest: &str) -> &str {
    if let Some(inner) = rest.strip_prefix('[') {
        return inner.split(']').next().unwrap_or_default();
    }
    rest.split(['/', ':']).next().unwrap_or_default()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A private-network IP literal: RFC 1918 IPv4 or an IPv6 unique-local address. Hostnames never
/// qualify (a name can resolve to a public address, or be rebound), nor do public, link-local or
/// unspecified addresses.
fn is_private_lan_ip(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_private(),
        Ok(std::net::IpAddr::V6(ip)) => (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// An explicit HTTP CONNECT proxy: where it is, how to authenticate to it, and which hosts
/// bypass it. The password is a [`Secret`]: it never appears in `Debug`, an error or a message.
#[derive(Clone, Debug)]
pub(crate) struct ProxySettings {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) credentials: Option<(String, Secret)>,
    pub(crate) no_proxy: Vec<String>,
}

impl ProxySettings {
    /// The `ureq` proxy these settings describe. Built per call, never stored, because its
    /// `Debug` would show the password.
    pub(crate) fn build(&self) -> Result<ureq::Proxy, ConfigError> {
        let mut builder = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
            .host(&self.host)
            .port(self.port);
        if let Some((username, password)) = &self.credentials {
            builder = builder.username(username).password(password.expose());
        }
        for entry in &self.no_proxy {
            builder = builder.no_proxy(entry);
        }
        builder
            .build()
            .map_err(|_| ConfigError("the proxy configuration is not valid".into()))
    }
}

/// Characters a proxy username or password may hold: they are placed in a URL's userinfo
/// unescaped, so anything that would change how that URL parses is refused.
fn userinfo_safe(value: &str, colon_allowed: bool) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || b"-._~!$&'()*+,;=".contains(&b)
                || (colon_allowed && b == b':')
        })
}

impl Common {
    pub(crate) fn new(
        base_url: &str,
        key: Option<Secret>,
        private_lan_http: bool,
    ) -> Result<Self, ConfigError> {
        let trimmed = base_url.trim().trim_end_matches('/');
        let mut insecure_lan = false;
        let rest = if let Some(rest) = trimmed.strip_prefix("https://") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("http://") {
            let host = host_of(rest);
            if is_loopback(host) {
                // Local inference on this machine.
            } else if private_lan_http && key.is_none() && is_private_lan_ip(host) {
                insecure_lan = true;
            } else {
                return Err(ConfigError(
                    "plain http is only allowed for a loopback host".into(),
                ));
            }
            rest
        } else {
            return Err(ConfigError("base_url must start with https://".into()));
        };
        if rest.is_empty() || rest.starts_with('/') || trimmed.contains(char::is_whitespace) {
            return Err(ConfigError("base_url has no host".into()));
        }
        if trimmed.contains('@') || trimmed.contains('?') || trimmed.contains('#') {
            return Err(ConfigError(
                "base_url must not carry credentials, a query or a fragment".into(),
            ));
        }
        if let Some(key) = &key
            && key.expose().is_empty()
        {
            return Err(ConfigError("api_key is empty".into()));
        }
        Ok(Self {
            base_url: trimmed.to_owned(),
            key,
            extra_headers: Vec::new(),
            connect_timeout: CONNECT_TIMEOUT,
            default_deadline: DEFAULT_DEADLINE,
            root_certs: None,
            proxy: None,
            insecure_lan,
        })
    }

    pub(crate) fn add_header(&mut self, name: &str, value: &str) -> Result<(), ConfigError> {
        let valid_name = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid_name {
            return Err(ConfigError(format!("{name:?} is not a valid header name")));
        }
        if self.insecure_lan {
            return Err(ConfigError(
                "a plain-http LAN endpoint takes no extra headers".into(),
            ));
        }
        if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(ConfigError(format!(
                "the {name} header is set by the adapter"
            )));
        }
        if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(ConfigError(
                "header values may not contain line breaks".into(),
            ));
        }
        self.extra_headers.push((name.to_owned(), value.to_owned()));
        Ok(())
    }

    pub(crate) fn set_root_certificates(&mut self, pem: &[u8]) -> Result<(), ConfigError> {
        let mut certs = Vec::new();
        for item in parse_pem(pem) {
            match item {
                Ok(PemItem::Certificate(cert)) => certs.push(cert),
                Ok(_) => {}
                Err(_) => {
                    return Err(ConfigError(
                        "the root certificate bundle is not valid PEM".into(),
                    ));
                }
            }
        }
        if certs.is_empty() {
            return Err(ConfigError(
                "the root certificate bundle holds no certificate".into(),
            ));
        }
        self.root_certs = Some(RootCerts::from(certs));
        Ok(())
    }

    pub(crate) fn set_proxy(&mut self, url: &str) -> Result<(), ConfigError> {
        if self.insecure_lan {
            return Err(ConfigError(
                "a plain-http LAN endpoint is not reached through a proxy".into(),
            ));
        }
        // Through a tunnel the proxy sees a plain-http request in full, credential included.
        if self.key.is_some() && self.base_url.starts_with("http://") {
            return Err(ConfigError(
                "a credential is never sent over plain http through a proxy".into(),
            ));
        }
        let rest = url
            .trim()
            .trim_end_matches('/')
            .strip_prefix("http://")
            .ok_or_else(|| {
                ConfigError("the proxy must be an http:// CONNECT proxy (http://host:port)".into())
            })?;
        if rest.contains(['@', '/', '?', '#']) || rest.contains(char::is_whitespace) {
            return Err(ConfigError(
                "the proxy URL is host:port only; credentials go through with_proxy_credentials"
                    .into(),
            ));
        }
        let (host, port) = rest
            .rsplit_once(':')
            .filter(|(host, _)| !host.is_empty())
            .ok_or_else(|| ConfigError("the proxy URL needs an explicit port".into()))?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| ConfigError("the proxy port is not valid".into()))?;
        let settings = ProxySettings {
            host: host.to_owned(),
            port,
            credentials: None,
            no_proxy: Vec::new(),
        };
        settings.build()?;
        self.proxy = Some(settings);
        Ok(())
    }

    pub(crate) fn set_proxy_credentials(
        &mut self,
        username: &str,
        password: Secret,
    ) -> Result<(), ConfigError> {
        let proxy = self
            .proxy
            .as_mut()
            .ok_or_else(|| ConfigError("set a proxy before its credentials".into()))?;
        if !userinfo_safe(username, false) || !userinfo_safe(password.expose(), true) {
            return Err(ConfigError(
                "proxy credentials may hold only letters, digits and -._~!$&'()*+,;= (and : in the password)"
                    .into(),
            ));
        }
        proxy.credentials = Some((username.to_owned(), password));
        Ok(())
    }

    pub(crate) fn add_no_proxy(&mut self, host: &str) -> Result<(), ConfigError> {
        let proxy = self
            .proxy
            .as_mut()
            .ok_or_else(|| ConfigError("set a proxy before its exceptions".into()))?;
        let bare = host.trim_start_matches("*.").trim_start_matches('.');
        if host == "*"
            || bare.is_empty()
            || !bare
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        {
            return Err(ConfigError(format!(
                "{host:?} is not a host, .domain or *.domain"
            )));
        }
        proxy.no_proxy.push(host.to_owned());
        Ok(())
    }

    pub(crate) fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }
}

macro_rules! shared_builders {
    () => {
        /// Sets the connect timeout.
        #[must_use]
        pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
            self.common.connect_timeout = timeout;
            self
        }

        /// Sets the deadline used when a request has no attempt budget.
        #[must_use]
        pub fn with_default_deadline(mut self, deadline: Duration) -> Self {
            self.common.default_deadline = deadline;
            self
        }

        /// Adds a request header, for example an attribution header. The adapter owns
        /// authentication, content type and the Anthropic version header, which cannot be set.
        pub fn with_header(mut self, name: &str, value: &str) -> Result<Self, ConfigError> {
            self.common.add_header(name, value)?;
            Ok(self)
        }

        /// Trusts exactly the certificates in `pem` (one or more `CERTIFICATE` blocks) in place
        /// of the bundled public roots, for an endpoint behind a TLS-intercepting proxy or signed
        /// by a private CA. Certificate validation stays on; other PEM items are ignored. The host
        /// reads the bundle (for example from `SSL_CERT_FILE`); this crate never touches the
        /// filesystem or the environment.
        pub fn with_root_certificates_pem(mut self, pem: &[u8]) -> Result<Self, ConfigError> {
            self.common.set_root_certificates(pem)?;
            Ok(self)
        }

        /// Reaches the endpoint through an HTTP CONNECT proxy at `url` (`http://host:port`).
        /// The tunnel carries the TLS session end to end; with a TLS-intercepting proxy, pass
        /// its CA with [`with_root_certificates_pem`](Self::with_root_certificates_pem). The
        /// host supplies the proxy explicitly; this crate never reads `HTTP_PROXY` or similar.
        /// A proxy failure is the call's failure: there is no direct connection as a fallback.
        /// A credential is never sent over plain `http` through a proxy.
        pub fn with_proxy(mut self, url: &str) -> Result<Self, ConfigError> {
            self.common.set_proxy(url)?;
            Ok(self)
        }

        /// Authenticates to the proxy (Basic). The password never appears in an error, a
        /// message or `Debug` output.
        pub fn with_proxy_credentials(
            mut self,
            username: &str,
            password: Secret,
        ) -> Result<Self, ConfigError> {
            self.common.set_proxy_credentials(username, password)?;
            Ok(self)
        }

        /// Connects to `host` directly instead of through the proxy: `example.com` exactly,
        /// or `.example.com` / `*.example.com` for its subdomains.
        pub fn with_no_proxy(mut self, host: &str) -> Result<Self, ConfigError> {
            self.common.add_no_proxy(host)?;
            Ok(self)
        }
    };
}

/// Settings for one OpenAI-compatible endpoint (OpenAI, OpenRouter, LM Studio, and others).
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    pub(crate) common: Common,
    pub(crate) template_thinking: Option<bool>,
}

impl OpenAiConfig {
    /// Default connect timeout.
    pub const CONNECT_TIMEOUT: Duration = CONNECT_TIMEOUT;
    /// Default deadline when a request carries no attempt budget.
    pub const DEFAULT_DEADLINE: Duration = DEFAULT_DEADLINE;

    /// An endpoint that needs a bearer token. `base_url` is the API root (for example
    /// `https://api.openai.com/v1` or `https://openrouter.ai/api/v1`); plain `http` is accepted
    /// only for a loopback host so a credential is never sent in the clear across a network.
    pub fn new(base_url: &str, api_key: Secret) -> Result<Self, ConfigError> {
        Ok(Self {
            common: Common::new(base_url, Some(api_key), false)?,
            template_thinking: None,
        })
    }

    /// An endpoint that takes no credential, such as a local LM Studio server
    /// (`http://localhost:1234/v1`). No `Authorization` header is sent.
    pub fn without_key(base_url: &str) -> Result<Self, ConfigError> {
        Ok(Self {
            common: Common::new(base_url, None, false)?,
            template_thinking: None,
        })
    }

    /// Like [`OpenAiConfig::without_key`], but also accepts a plain-`http` endpoint at a
    /// private-network IP literal (`192.168.x.x`, `10.x.x.x`, `172.16-31.x.x`, or an IPv6
    /// unique-local address), for a keyless inference server elsewhere on the same LAN.
    ///
    /// This is deliberately narrow: it exists only on this keyless constructor, so no credential
    /// can be configured with it; such a configuration accepts no extra headers; hostnames and
    /// public addresses are still refused; and redirects are never followed. The host must opt in
    /// explicitly; this crate never reads the environment.
    pub fn without_key_on_private_lan(base_url: &str) -> Result<Self, ConfigError> {
        Ok(Self {
            common: Common::new(base_url, None, true)?,
            template_thinking: None,
        })
    }

    /// Sends `"chat_template_kwargs":{"enable_thinking":<enable>}` at the top level of every
    /// request body, which is where servers that render a chat template (LM Studio, vLLM and
    /// similar, for models such as Qwen) read it. This is a host setting for that vendor
    /// extension, not FX's reasoning effort, and it is off unless set because servers that do
    /// not know the field may reject it.
    #[must_use]
    pub fn with_chat_template_thinking(mut self, enable: bool) -> Self {
        self.template_thinking = Some(enable);
        self
    }

    shared_builders!();
}

/// Settings for the Anthropic Messages API.
#[derive(Clone, Debug)]
pub struct AnthropicConfig {
    pub(crate) common: Common,
    pub(crate) version: String,
    pub(crate) default_max_tokens: u32,
}

impl AnthropicConfig {
    /// The API version header Anthropic documents, used unless the host sets another.
    pub const VERSION: &'static str = "2023-06-01";
    /// Anthropic requires `max_tokens`. This is the value used when the request does not carry
    /// one; it is a host setting, never a hidden constant.
    pub const DEFAULT_MAX_TOKENS: u32 = 4096;

    /// `base_url` is the API root, for example `https://api.anthropic.com/v1`.
    pub fn new(base_url: &str, api_key: Secret) -> Result<Self, ConfigError> {
        Ok(Self {
            common: Common::new(base_url, Some(api_key), false)?,
            version: Self::VERSION.to_owned(),
            default_max_tokens: Self::DEFAULT_MAX_TOKENS,
        })
    }

    /// Sets the `anthropic-version` header.
    pub fn with_version(mut self, version: &str) -> Result<Self, ConfigError> {
        if version.is_empty()
            || version
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(ConfigError("the Anthropic version is not valid".into()));
        }
        self.version = version.to_owned();
        Ok(self)
    }

    /// Sets the `max_tokens` used when a request does not carry `max_output_tokens`.
    #[must_use]
    pub fn with_default_max_tokens(mut self, tokens: u32) -> Self {
        self.default_max_tokens = tokens.max(1);
        self
    }

    shared_builders!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lan(url: &str) -> Result<OpenAiConfig, ConfigError> {
        OpenAiConfig::without_key_on_private_lan(url)
    }

    /// A self-signed P-256 root used only as parse input; nothing here connects with it.
    const TEST_ROOT: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBpTCCAUugAwIBAgIUGxuR7UMf4wCGUFknHqHQLWt5m8UwCgYIKoZIzj0EAwIw\n\
JzElMCMGA1UEAwwcZngtbmF0aXZlLXByb3ZpZGVyIHRlc3Qgcm9vdDAgFw0yNjEw\n\
MTYyMzMzMDdaGA8yMTI2MDkyMjIzMzMwN1owJzElMCMGA1UEAwwcZngtbmF0aXZl\n\
LXByb3ZpZGVyIHRlc3Qgcm9vdDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABD/G\n\
kpsDC8dks62rbWrLyQOKfPkRE8bJO5zo34h/jpjW6+Bk4E0NxvHG+/Pk5XnE0Kl4\n\
6ZLRPS23H2e67YN8ey+jUzBRMB0GA1UdDgQWBBSGpTf0iUNc8ZX9GGBfjFmYkX+U\n\
0DAfBgNVHSMEGDAWgBSGpTf0iUNc8ZX9GGBfjFmYkX+U0DAPBgNVHRMBAf8EBTAD\n\
AQH/MAoGCCqGSM49BAMCA0gAMEUCIQDnh3EtlQ2kBCfUMrFfqiqIhT/VVZFkqUV+\n\
iZCe/neitgIgb1IFqNXOjw0/7R5oHwzaJiUCaJ+R5Waxy1oxMw8U2LM=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn plain_http_to_a_non_loopback_host_is_refused_by_default() {
        for url in [
            "http://192.168.1.20:1234/v1",
            "http://10.0.0.5/v1",
            "http://lmstudio.local:1234/v1",
            "http://8.8.8.8/v1",
        ] {
            assert!(OpenAiConfig::without_key(url).is_err(), "{url}");
            assert!(
                OpenAiConfig::new(url, Secret::new("k")).is_err(),
                "{url} with a key"
            );
        }
    }

    #[test]
    fn loopback_http_works_without_the_opt_in_in_every_spelling() {
        for url in [
            "http://127.0.0.1:1234/v1",
            "http://localhost:1234/v1",
            "http://[::1]:1234/v1",
            "http://127.0.0.2/v1",
        ] {
            assert!(OpenAiConfig::without_key(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn the_opt_in_accepts_private_ip_literals_only() {
        for url in [
            "http://10.1.2.3:1234/v1",
            "http://172.16.0.9/v1",
            "http://172.31.255.1:1234/v1",
            "http://192.168.1.20:1234/v1",
            "http://[fd12:3456::1]:1234/v1",
        ] {
            assert!(lan(url).is_ok(), "{url}");
        }
        for url in [
            "http://8.8.8.8:1234/v1",
            "http://172.32.0.1/v1",
            "http://172.15.0.1/v1",
            "http://169.254.1.1/v1",
            "http://0.0.0.0:1234/v1",
            "http://[fe80::1]/v1",
            "http://[2001:db8::1]/v1",
            "http://lmstudio.local:1234/v1",
            "http://example.com/v1",
            "http://192.168.1.20.example.com/v1",
        ] {
            assert!(lan(url).is_err(), "{url}");
        }
    }

    #[test]
    fn a_credential_never_goes_with_plain_http_off_loopback_even_through_the_internal_path() {
        // The only constructor that takes the exception takes no key, and `new` with a key refuses
        // every non-loopback http endpoint, opt-in or not.
        assert!(Common::new("http://192.168.1.20/v1", Some(Secret::new("k")), true).is_err());
        assert!(Common::new("http://192.168.1.20/v1", None, true).is_ok());
    }

    #[test]
    fn a_lan_configuration_accepts_no_extra_headers() {
        let config = lan("http://192.168.1.20:1234/v1").unwrap();
        assert!(config.clone().with_header("X-Api-Key", "secret").is_err());
        assert!(
            config
                .with_header("HTTP-Referer", "https://example.test")
                .is_err()
        );
        // Ordinary configurations are unaffected.
        let ok = OpenAiConfig::without_key("https://openrouter.ai/api/v1").unwrap();
        assert!(
            ok.with_header("HTTP-Referer", "https://example.test")
                .is_ok()
        );
    }

    #[test]
    fn https_and_loopback_configurations_do_not_become_lan_configurations() {
        let config = lan("http://127.0.0.1:1234/v1").unwrap();
        assert!(config.with_header("X-Title", "t").is_ok());
    }

    #[test]
    fn a_proxy_is_explicit_validated_and_never_carries_a_credential_in_the_clear() {
        let https = || OpenAiConfig::new("https://openrouter.ai/api/v1", Secret::new("k")).unwrap();
        assert!(
            https().common.proxy.is_none(),
            "no proxy unless the host sets one"
        );
        let proxied = https()
            .with_proxy("http://proxy.internal:3128")
            .unwrap()
            .with_proxy_credentials("svc", Secret::new("p4ss:word"))
            .unwrap()
            .with_no_proxy(".corp.example")
            .unwrap();
        let proxy = proxied.common.proxy.as_ref().unwrap();
        assert_eq!((proxy.host.as_str(), proxy.port), ("proxy.internal", 3128));
        assert!(proxy.build().is_ok());
        assert!(!format!("{proxied:?}").contains("p4ss:word"));

        for url in [
            "https://proxy:3128",
            "socks5://proxy:1080",
            "http://proxy",
            "http://proxy:0",
            "http://user:pw@proxy:3128",
            "http://proxy:3128/path",
            "proxy:3128",
        ] {
            assert!(https().with_proxy(url).is_err(), "{url}");
        }
        assert!(
            https()
                .with_proxy_credentials("u", Secret::new("p"))
                .is_err(),
            "credentials need a proxy"
        );
        assert!(
            https().with_no_proxy("x.example").is_err(),
            "exceptions need a proxy"
        );
        for (user, pass) in [
            ("a@b", "p"),
            ("a:b", "p"),
            ("u", "p/w"),
            ("u", "p@w"),
            ("", "p"),
        ] {
            assert!(
                https()
                    .with_proxy("http://proxy:3128")
                    .unwrap()
                    .with_proxy_credentials(user, Secret::new(pass))
                    .is_err(),
                "{user}:{pass}"
            );
        }
        for host in ["*", "", "a b", "a/b"] {
            assert!(
                https()
                    .with_proxy("http://proxy:3128")
                    .unwrap()
                    .with_no_proxy(host)
                    .is_err(),
                "{host:?}"
            );
        }
        // A key over plain http would be visible to the proxy; a keyless loopback is not.
        let keyed_http = OpenAiConfig::new("http://127.0.0.1:1234/v1", Secret::new("k")).unwrap();
        assert!(keyed_http.with_proxy("http://proxy:3128").is_err());
        let keyless_http = OpenAiConfig::without_key("http://127.0.0.1:1234/v1").unwrap();
        assert!(keyless_http.with_proxy("http://proxy:3128").is_ok());
        assert!(
            lan("http://192.168.1.20:1234/v1")
                .unwrap()
                .with_proxy("http://proxy:3128")
                .is_err()
        );
    }

    #[test]
    fn bundled_roots_are_the_default() {
        let config = OpenAiConfig::without_key("https://openrouter.ai/api/v1").unwrap();
        assert!(config.common.root_certs.is_none());
    }

    #[test]
    fn a_pem_bundle_replaces_the_bundled_roots() {
        let two = format!("# comment\n{TEST_ROOT}\n{TEST_ROOT}");
        let config = OpenAiConfig::new("https://ai-gateway.vercel.sh/v1", Secret::new("k"))
            .unwrap()
            .with_root_certificates_pem(two.as_bytes())
            .unwrap();
        match config.common.root_certs {
            Some(RootCerts::Specific(certs)) => assert_eq!(certs.len(), 2),
            other => panic!("expected specific roots, got {other:?}"),
        }
        let anthropic = AnthropicConfig::new("https://api.anthropic.com/v1", Secret::new("k"))
            .unwrap()
            .with_root_certificates_pem(TEST_ROOT.as_bytes())
            .unwrap();
        assert!(anthropic.common.root_certs.is_some());
    }

    #[test]
    fn a_bundle_without_a_valid_certificate_is_refused() {
        let config = OpenAiConfig::without_key("https://openrouter.ai/api/v1").unwrap();
        let corrupt = TEST_ROOT.replace("MIIB", "M!IB");
        for pem in [
            "",
            "not a certificate",
            "-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----\n",
            corrupt.as_str(),
        ] {
            assert!(
                config
                    .clone()
                    .with_root_certificates_pem(pem.as_bytes())
                    .is_err(),
                "{pem:?}"
            );
        }
    }
}
