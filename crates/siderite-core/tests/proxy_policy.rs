//! Proxy spoofing, canonical redirect authority and normalized limiter keys.

use http::{Request, StatusCode, header};
use siderite_core::{App, Body, HttpsRedirect as Redirect, PlainText, get};
use siderite_core::{RateLimit, TrustedHosts, TrustedProxies};
use siderite_testkit::{TestClient as Client, TestResponse};
use std::net::{IpAddr, SocketAddr};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fields<'a> = [(&'a str, &'a str)];
type ClientResult = Result<Client, siderite_core::ServerError>;
type Reply = Result<TestResponse, Box<dyn std::error::Error>>;

async fn hello() -> PlainText<&'static str> {
    PlainText("ok")
}

fn trust() -> TrustedProxies {
    TrustedProxies::new([IpAddr::from([127, 0, 0, 1])])
}

async fn ask(c: &Client, peer: Option<&str>, fields: &Fields<'_>) -> Reply {
    let mut request = Request::builder().uri("/path?x=1").body(Body::empty())?;
    for (name, value) in fields {
        request
            .headers_mut()
            .append(name.parse::<header::HeaderName>()?, value.parse()?);
    }
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer.parse::<SocketAddr>()?));
    }
    Ok(c.send(request).await?)
}

fn app() -> App {
    App::new().route("/path", get(hello))
}

fn redirect_client(policy: Redirect) -> ClientResult {
    Client::try_new(app().https_redirect(policy))
}

#[tokio::test]
async fn untrusted_scheme_spoofing_is_ignored() -> TestResult {
    let redirect = Redirect::new().trusted_proxies(trust());
    let client = Client::try_new(app().https_redirect(redirect))?;
    let fields = [("host", "example.com"), ("x-forwarded-proto", "https")];
    for peer in [None, Some("127.0.0.2:4000")] {
        let reply = ask(&client, peer, &fields).await?;
        assert_eq!(reply.status, StatusCode::PERMANENT_REDIRECT);
    }
    let reply = ask(&client, Some("127.0.0.1:4000"), &fields).await?;
    assert_eq!(reply.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn blanket_scheme_trust_requires_explicit_opt_in() -> TestResult {
    let fields = [("host", "example.com"), ("x-forwarded-proto", "https")];
    let safe = redirect_client(Redirect::new())?;
    assert_eq!(
        ask(&safe, None, &fields).await?.status,
        StatusCode::PERMANENT_REDIRECT
    );
    let policy = Redirect::new().trust_forwarded_proto(true);
    let compatible = redirect_client(policy)?;
    assert_eq!(
        ask(&compatible, None, &fields).await?.status,
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn invalid_trusted_scheme_headers() -> TestResult {
    let redirect = Redirect::new().trusted_proxies(trust());
    let client = Client::try_new(app().https_redirect(redirect))?;
    for values in [vec!["https,http"], vec!["ftp"], vec!["https", "http"]] {
        let mut fields = vec![("host", "example.com")];
        for value in values {
            fields.push(("x-forwarded-proto", value));
        }
        let reply = ask(&client, Some("127.0.0.1:4000"), &fields).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    Ok(())
}

#[tokio::test]
async fn ipv6_brackets_and_canonical_https_port_are_preserved() -> TestResult {
    let client = redirect_client(Redirect::new())?;
    let reply = ask(&client, None, &[("host", "[::1]:8080")]).await?;
    let location = reply
        .headers
        .get(header::LOCATION)
        .ok_or("missing redirect location")?
        .to_str()?;
    assert_eq!(location, "https://[::1]/path?x=1");
    let parsed = location.parse::<http::Uri>()?;
    assert_eq!(parsed.host(), Some("[::1]"));
    let authority = "api.example.com:8443".parse()?;
    let canonical = Redirect::new().canonical_authority(authority);
    let client = Client::try_new(app().https_redirect(canonical))?;
    let reply = ask(&client, None, &[("host", "[::1]:8080")]).await?;
    assert_eq!(
        reply
            .headers
            .get(header::LOCATION)
            .ok_or("missing canonical location")?
            .to_str()?,
        "https://api.example.com:8443/path?x=1"
    );
    Ok(())
}

#[tokio::test]
async fn invalid_or_disallowed_hosts() -> TestResult {
    let redirect = Redirect::new()
        .trusted_proxies(trust())
        .allowed_hosts(TrustedHosts::new(["example.com", "*.example.com"]));
    let client = Client::try_new(app().https_redirect(redirect))?;
    let hosts = ["evil.test", "u@example.com", "[::1", "example.com:65536"];
    for host in hosts {
        let fields = [("host", host), ("x-forwarded-proto", "https")];
        let reply = ask(&client, Some("127.0.0.1:4000"), &fields).await?;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    let fields = [("host", "example.com"), ("host", "evil.test")];
    assert_eq!(
        ask(&client, None, &fields).await?.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ask(&client, None, &[("host", "API.Example.com:80")])
            .await?
            .status,
        StatusCode::PERMANENT_REDIRECT
    );
    Ok(())
}

#[tokio::test]
async fn untrusted_client_ip_cannot_create_new_buckets() -> TestResult {
    let limiter = RateLimit::new(1, 0.001).trusted_proxies(trust());
    let client = Client::try_new(app().rate_limit(limiter))?;
    let peer = Some("127.0.0.2:4000");
    assert_eq!(
        ask(&client, peer, &[("x-forwarded-for", "1.1.1.1")])
            .await?
            .status,
        StatusCode::OK
    );
    assert_eq!(
        ask(&client, peer, &[("x-forwarded-for", "2.2.2.2")])
            .await?
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    Ok(())
}

#[tokio::test]
async fn normalized_forwarded_ips_share_a_bucket() -> TestResult {
    let limiter = RateLimit::new(1, 0.001).trusted_proxies(trust());
    let client = Client::try_new(app().rate_limit(limiter))?;
    let peer = Some("127.0.0.1:4000");
    let a = [("x-forwarded-for", "2001:0db8:0:0:0:0:0:1")];
    let b = [("x-forwarded-for", "2001:db8::1")];
    assert_eq!(ask(&client, peer, &a).await?.status, StatusCode::OK);
    assert_eq!(
        ask(&client, peer, &b).await?.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let a = [("x-forwarded-for", "::ffff:10.0.0.1")];
    let b = [("x-forwarded-for", "10.0.0.1")];
    assert_eq!(ask(&client, peer, &a).await?.status, StatusCode::OK);
    assert_eq!(
        ask(&client, peer, &b).await?.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    Ok(())
}

#[tokio::test]
async fn trusted_ip_chains_and_malformed_values_are_rejected() -> TestResult {
    let limiter = RateLimit::new(1, 0.001).trusted_proxies(trust());
    let client = Client::try_new(app().rate_limit(limiter))?;
    let peer = Some("127.0.0.1:4000");
    for ip in ["arbitrary-key", "1.1.1.1, 2.2.2.2", "", "[::1]"] {
        assert_eq!(
            ask(&client, peer, &[("x-forwarded-for", ip)]).await?.status,
            StatusCode::BAD_REQUEST
        );
    }
    let duplicates = [
        ("x-forwarded-for", "1.1.1.1"),
        ("x-forwarded-for", "2.2.2.2"),
    ];
    assert_eq!(
        ask(&client, peer, &duplicates).await?.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ask(&client, peer, &[("x-forwarded-for", "1.1.1.1")])
            .await?
            .status,
        StatusCode::OK
    );
    Ok(())
}

#[test]
fn invalid_limiter_configuration_fails_app_validation() {
    for rate in [0.0, f64::NAN, f64::INFINITY] {
        assert!(
            app()
                .rate_limit(RateLimit::new(1, rate))
                .into_router_service()
                .is_err()
        );
    }
    assert!(
        app()
            .rate_limit(RateLimit::new(0, 1.0))
            .into_router_service()
            .is_err()
    );
    assert!(
        app()
            .rate_limit(RateLimit::new(1, 1.0).max_clients(0))
            .into_router_service()
            .is_err()
    );
}
