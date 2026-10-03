//! Host validation and HTTPS redirection.

use super::TrustedProxies;
use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::body::Body;
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::{IntoResponse, Response};
use http::uri::Authority;
use http::{HeaderValue, StatusCode, header};
use std::sync::Arc;
use tower::Layer;

fn bad_request(message: &str) -> Response {
    ApiError::bad_request(message).into_response()
}

/// Parse one request authority, excluding user info and invalid ports.
fn request_authority(req: &Request) -> Option<Authority> {
    let mut hosts = req.headers().get_all(header::HOST).iter();
    let raw = match hosts.next() {
        Some(value) => value.to_str().ok()?,
        None => req.uri().authority()?.as_str(),
    };
    if hosts.next().is_some() {
        return None;
    }
    let authority = raw.parse::<Authority>().ok()?;
    valid_authority(&authority).then_some(authority)
}

fn valid_authority(authority: &Authority) -> bool {
    let raw = authority.as_str();
    let host = authority.host();
    if raw.contains('@') || host.is_empty() {
        return false;
    }
    raw.strip_prefix(host).is_some_and(|suffix| {
        suffix.is_empty()
            || suffix
                .strip_prefix(':')
                .is_some_and(|port| port.parse::<u16>().is_ok())
    })
}

fn request_host(req: &Request) -> Option<String> {
    let authority = request_authority(req)?;
    Some(
        authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase(),
    )
}

/// Rejects requests whose `Host` is not on an allow-list (400 problem).
///
/// Patterns are exact hosts (`example.com`), subdomain wildcards
/// (`*.example.com`, which does not match the apex) or `*`. Matching is
/// case-insensitive and ignores the port.
#[derive(Debug, Clone)]
pub struct TrustedHosts {
    patterns: Arc<[String]>,
}

impl TrustedHosts {
    /// Allow the given host patterns.
    pub fn new<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let patterns: Vec<String> = patterns
            .into_iter()
            .map(|p| p.as_ref().to_ascii_lowercase())
            .collect();
        Self {
            patterns: patterns.into(),
        }
    }

    fn allows(&self, host: &str) -> bool {
        self.patterns.iter().any(|p| match p.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|head| head.ends_with('.') && head.len() > 1),
            None => p == "*" || p == host,
        })
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let allowed = request_host(&req).is_some_and(|h| this.allows(&h));
            async move {
                if allowed {
                    next.run(req).await
                } else {
                    bad_request("Invalid or missing Host header.")
                }
            }
        })
        .layer(inner)
    }
}

/// Redirect plain HTTP to HTTPS with an explicit proxy and authority policy.
///
/// Forwarded scheme headers are ignored by default. Configure exact trusted
/// socket peers with [`Self::trusted_proxies`]; those proxies must replace
/// client-supplied forwarded headers. Incoming HTTP ports are removed by
/// default, preserving IPv6 brackets. Set a canonical authority for a custom
/// HTTPS port or deployment host, and an allow-list for incoming hosts.
#[derive(Debug, Clone, Default)]
pub struct HttpsRedirect {
    proxies: TrustedProxies,
    canonical: Option<Authority>,
    allowed: Option<TrustedHosts>,
}

impl HttpsRedirect {
    /// Redirect while ignoring untrusted forwarded headers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure exact trusted proxy peers using the shared policy.
    ///
    /// Args:
    ///     proxies: Immediate peers required to replace forwarded headers.
    ///
    /// Returns:
    ///     Redirect middleware with explicit forwarded-scheme trust.
    #[must_use]
    pub fn trusted_proxies(mut self, proxies: TrustedProxies) -> Self {
        self.proxies = proxies;
        self
    }

    /// Explicit compatibility trust for forwarded scheme from any source.
    ///
    /// Args:
    ///     trust: Blanket ingress trust; false restores the safe default.
    ///
    /// Returns:
    ///     Middleware using the selected forwarded-scheme policy.
    #[must_use]
    pub fn trust_forwarded_proto(self, trust: bool) -> Self {
        self.trusted_proxies(if trust {
            TrustedProxies::dangerously_trust_all()
        } else {
            TrustedProxies::default()
        })
    }

    /// Set an explicit HTTPS destination, including an optional port.
    ///
    /// Args:
    ///     authority: Parsed destination without user info.
    ///
    /// Returns:
    ///     Middleware with a fixed destination; invalid user info yields 400.
    #[must_use]
    pub fn canonical_authority(mut self, authority: Authority) -> Self {
        self.canonical = Some(authority);
        self
    }

    /// Validate incoming hosts before redirecting or invoking the origin.
    ///
    /// Args:
    ///     hosts: Case-insensitive host allow-list with wildcard support.
    ///
    /// Returns:
    ///     Middleware rejecting missing, malformed or disallowed hosts.
    #[must_use]
    pub fn allowed_hosts(mut self, hosts: TrustedHosts) -> Self {
        self.allowed = Some(hosts);
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let secure = this
                .proxies
                .resolve(&req)
                .map(|context| context.scheme == "https");
            let incoming = request_authority(&req);
            let host = request_host(&req);
            let allowed = match this.allowed.as_ref() {
                Some(p) => host.as_ref().is_some_and(|h| p.allows(h)),
                None => true,
            };
            let canonical = this.canonical.clone();
            async move {
                if !allowed {
                    return bad_request("Invalid or disallowed Host header.");
                }
                let secure = match secure {
                    Ok(secure) => secure,
                    Err(_) => {
                        return bad_request("Invalid forwarded scheme.");
                    }
                };
                if secure {
                    return next.run(req).await;
                }
                let Some(incoming) = incoming else {
                    return bad_request("Invalid or missing Host header.");
                };
                let host = match canonical.as_ref() {
                    Some(value) if valid_authority(value) => value.as_str(),
                    Some(_) => {
                        return bad_request("Invalid canonical authority.");
                    }
                    None => incoming.host(),
                };
                let path = req.uri().path_and_query();
                let target = path.map_or("/", |pq| pq.as_str());
                let uri = format!("https://{host}{target}");
                match HeaderValue::from_str(&uri) {
                    Ok(location) => {
                        let mut response = Response::new(Body::empty());
                        let status = StatusCode::PERMANENT_REDIRECT;
                        *response.status_mut() = status;
                        let headers = response.headers_mut();
                        headers.insert(header::LOCATION, location);
                        response
                    }
                    Err(_) => bad_request("Invalid redirect target."),
                }
            }
        })
        .layer(inner)
    }
}

impl_layer!(TrustedHosts, HttpsRedirect);
