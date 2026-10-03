# Database transport verification

Enable `tls` on `siderite-backends` for SQLx PostgreSQL/MySQL and Redis
Rustls transport. Redis cache users enable `redis,tls` on `siderite-cache`.
The base SQLx/Redis features do not compile TLS support. Enabling transport
support does not choose a verifying mode or authenticate an arbitrary peer.

| Adapter | Verified configuration |
|---|---|
| SQLx PostgreSQL | URL `sslmode=verify-full`; `sslrootcert` for a private CA |
| SQLx MySQL | URL `ssl-mode=VERIFY_IDENTITY`; `ssl-ca` for a private CA |
| Native MySQL | `connect_options(Opts, limits)` with `SslOpts` and roots |
| Redis | `rediss://`; configured Client roots via `RedisStore::connect_with` |
| MongoDB | Driver Rustls is already enabled; `tls=true` plus CA/hostname policy |
| SQLite | Local file security; no network TLS transport |

SQLx's preferred modes may fall back to unencrypted transport and do not
provide the identity verification above. URL-encode certificate paths and
credentials; keep URLs out of logs. External SQLx pools retain their own
TLS/session policy. A hostname must match the certificate SAN.

Native MySQL already compiles Rustls. Its URL `require_ssl=true` enables
default verification using built-in roots; private roots need native typed
options. Keep certificate and domain verification enabled. `connect_options`
preserves TLS options but overrides native pool bounds, session setup,
found-rows and statement-cache policy to retain adapter contracts.

```rust
use mysql_async::{Opts, OptsBuilder, SslOpts};
use siderite_backends::mysql::native::{
    NativeMySqlBackend, NativeMySqlOptions,
};

let tls = SslOpts::default()
    .with_root_certs(vec![private_ca_pem.into()])
    .with_disable_built_in_roots(true);
let connection = OptsBuilder::from_opts(Opts::from_url(&database_url)?)
    .ssl_opts(tls).into();
let backend = NativeMySqlBackend::connect_options(
    connection, NativeMySqlOptions::default(),
).await?;
```

Redis `Client::build_with_tls` accepts `TlsCertificates` for custom trust
roots/client identity. Pass the client and `ConnectionManagerConfig` to
`RedisStore::connect_with`; configure bounded reconnect attempts, connection
and response timeouts for the deployment. Avoid verification-disable flags.
MongoDB's driver options must also retain certificate/hostname verification;
its `tlsAllowInvalidCertificates` and `tlsAllowInvalidHostnames` weaken that
policy.

`python3 scripts/test_tls.py` creates its own CA and localhost-only server
certificate, starts four uniquely named loopback-only disposable services,
and runs the explicit ignored TLS contract. It checks PostgreSQL, SQLx and
native MySQL, Redis and MongoDB against valid TLS, an untrusted root and a
mismatched hostname. MySQL, Redis and MongoDB fixtures reject plaintext.
The script removes only
its own containers and temporary keys, including failure paths. It requires
Docker and OpenSSL. The resulting `target/tls-contract.json` contains no URLs,
passwords or private keys. It is a local acceptance record, not a production
certificate or a measurement of TLS overhead.

Both live contracts passed on 2026-10-03 with PostgreSQL 17, MySQL 8.4,
Redis 7 and MongoDB 8 disposable fixtures. PostgreSQL reports TLS through
`pg_stat_ssl`; both MySQL adapters report a nonempty negotiated cipher.
Redis and MongoDB accept only encrypted fixture connections. The wrong-root
and wrong-hostname attempts fail with bounded connection deadlines.
