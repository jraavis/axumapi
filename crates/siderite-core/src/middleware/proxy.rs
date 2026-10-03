//! Explicit socket-peer trust for forwarded request context.

use crate::extract::Request;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

type ClientIp = Result<Option<IpAddr>, ()>;
type Header<'a> = Result<Option<&'a str>, ()>;

/// Validated client and transport context from one proxy policy.
#[derive(Debug, Clone, Copy)]
pub struct ProxyContext {
    /// Normalized client address, or no address when the peer is unknown.
    pub client_ip: Option<IpAddr>,
    /// Effective request scheme (`http` or `https`).
    pub scheme: &'static str,
}

/// Invalid or ambiguous headers supplied by a trusted proxy peer.
#[derive(Debug, thiserror::Error)]
#[error("invalid forwarded request context")]
pub struct ProxyError;

/// Proxy peers permitted to supply forwarded client and scheme headers.
///
/// Defaults to no trust. Exact IP addresses are supported; configure the
/// immediate trusted peer to overwrite headers rather than append client
/// input. This policy deliberately does not interpret arbitrary proxy chains.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    peers: Arc<HashSet<IpAddr>>,
    all: bool,
}

impl TrustedProxies {
    /// Trust only the supplied socket-peer IP addresses.
    ///
    /// Args:
    ///     peers: Exact addresses of proxies that replace forwarded headers.
    ///
    /// Returns:
    ///     A policy that ignores headers from other or unknown peers.
    pub fn new(peers: impl IntoIterator<Item = IpAddr>) -> Self {
        let peers = peers.into_iter().map(|ip| ip.to_canonical()).collect();
        Self {
            peers: Arc::new(peers),
            all: false,
        }
    }

    /// Explicit compatibility policy trusting all forwarded header sources.
    ///
    /// Args:
    ///     None.
    ///
    /// Returns:
    ///     Blanket trust, including requests without socket-peer metadata.
    ///     Use only when ingress guarantees clients cannot forge these fields.
    pub fn dangerously_trust_all() -> Self {
        Self {
            peers: Arc::default(),
            all: true,
        }
    }

    /// Resolve client and scheme consistently across middleware and caching.
    ///
    /// Args:
    ///     req: Request with optional socket-peer metadata.
    ///
    /// Returns:
    ///     Validated context, or a malformed trusted-header error.
    pub fn resolve(&self, req: &Request) -> Result<ProxyContext, ProxyError> {
        let client_ip = self.client_ip(req).map_err(|_| ProxyError)?;
        let secure = self.is_https(req).map_err(|_| ProxyError)?;
        Ok(ProxyContext {
            client_ip,
            scheme: if secure { "https" } else { "http" },
        })
    }

    pub(crate) fn accepts(&self, req: &Request) -> bool {
        self.all || peer_ip(req).is_some_and(|ip| self.peers.contains(&ip))
    }

    pub(crate) fn client_ip(&self, req: &Request) -> ClientIp {
        if !self.accepts(req) {
            return Ok(peer_ip(req));
        }
        let Some(value) = single_header(req, "x-forwarded-for")? else {
            return Ok(peer_ip(req));
        };
        // Only one validated client IP is accepted under exact-peer trust.
        // Blanket compatibility mode retains the documented first-hop rule.
        let value = if self.all {
            value.split(',').next().ok_or(())?
        } else {
            if value.contains(',') {
                return Err(());
            }
            value
        };
        value
            .trim()
            .parse::<IpAddr>()
            .map(|ip| Some(ip.to_canonical()))
            .map_err(|_| ())
    }

    pub(crate) fn is_https(&self, req: &Request) -> Result<bool, ()> {
        if req.uri().scheme_str() == Some("https") {
            return Ok(true);
        }
        if !self.accepts(req) {
            return Ok(false);
        }
        match single_header(req, "x-forwarded-proto")? {
            None => Ok(false),
            Some(value) if value.eq_ignore_ascii_case("https") => Ok(true),
            Some(value) if value.eq_ignore_ascii_case("http") => Ok(false),
            Some(_) => Err(()),
        }
    }
}

fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_canonical())
}

fn single_header<'a>(req: &'a Request, name: &str) -> Header<'a> {
    let mut values = req.headers().get_all(name).iter();
    let first = values
        .next()
        .map(|value| value.to_str().map_err(|_| ()))
        .transpose()?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}
