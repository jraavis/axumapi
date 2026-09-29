//! Host validation and HTTPS redirection.

use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::body::Body;
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use std::sync::Arc;
use tower::Layer;

/// The request's host (without port), from `Host` or the URI authority.
fn request_host(req: &Request) -> Option<String> {
    let raw = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().authority().map(http::uri::Authority::as_str))?;
    Some(strip_port(raw).to_ascii_lowercase())
}

fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(authority);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
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
                    ApiError::bad_request("Invalid or missing Host header.").into_response()
                }
            }
        })
        .layer(inner)
    }
}

/// Redirects plain-HTTP requests to HTTPS with `308 Permanent Redirect`.
///
/// A request counts as HTTPS if `X-Forwarded-Proto: https` is present (only
/// when [`trust_forwarded_proto`](Self::trust_forwarded_proto) is on, the
/// default, because TLS is usually terminated by a proxy) or its URI scheme is
/// `https`. Requests without a `Host` header get a 400 problem.
#[derive(Debug, Clone)]
pub struct HttpsRedirect {
    trust_forwarded_proto: bool,
}

impl Default for HttpsRedirect {
    fn default() -> Self {
        Self {
            trust_forwarded_proto: true,
        }
    }
}

impl HttpsRedirect {
    /// Redirect using the default (proxy-aware) policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to believe `X-Forwarded-Proto`. Turn off when clients can reach
    /// the app directly, since the header could be forged.
    #[must_use]
    pub fn trust_forwarded_proto(mut self, trust: bool) -> Self {
        self.trust_forwarded_proto = trust;
        self
    }

    fn is_https(&self, req: &Request) -> bool {
        let forwarded = self.trust_forwarded_proto
            && req
                .headers()
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("https"));
        forwarded || req.uri().scheme_str() == Some("https")
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let secure = this.is_https(&req);
            async move {
                if secure {
                    return next.run(req).await;
                }
                let Some(host) = request_host(&req) else {
                    return ApiError::bad_request("Missing Host header.").into_response();
                };
                let target = req.uri().path_and_query().map_or("/", |pq| pq.as_str());
                match HeaderValue::from_str(&format!("https://{host}{target}")) {
                    Ok(location) => {
                        let mut response = Response::new(Body::empty());
                        *response.status_mut() = StatusCode::PERMANENT_REDIRECT;
                        response.headers_mut().insert(header::LOCATION, location);
                        response
                    }
                    Err(_) => ApiError::bad_request("Invalid redirect target.").into_response(),
                }
            }
        })
        .layer(inner)
    }
}

impl_layer!(TrustedHosts, HttpsRedirect);
