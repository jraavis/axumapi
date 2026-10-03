# Dependency exposure decisions

## RUSTSEC-2023-0071: rsa

Reviewed: 2026-10-03. Locked packages: rsa 0.9.10, sqlx-mysql 0.8.6.
Responsible role: backend/release maintainers; no named assignee is recorded.
Review on every relevant lockfile/callpath change and before a release.

[RustSec](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) still lists
no patched versions. The advisory concerns timing disclosure from private-key
operations. An advisory exception is not a claim that the dependency is safe
for arbitrary cryptographic use.

The inspected framework and benchmark Rust sources have no `RsaPrivateKey`
use or direct rsa signing/decryption call. SQLx MySQL authentication imports
`RsaPublicKey` and performs OAEP public-key encryption of the password in
`connection/auth.rs::encrypt_rsa` for SHA256/caching-SHA2 authentication over
an unencrypted connection. The server supplies that public key. The TLS path
sends the authentication password inside the encrypted transport and skips
this RSA exchange. No affected client private-key operation was found in
this pinned driver callpath; this does not audit the database server.

Decision: retain the narrow cargo-deny exception while this is the only
reviewed rsa exposure. Use certificate- and hostname-verified TLS for remote
databases. Public-key password encryption without verified transport does
not authenticate the database peer. Application dependencies adding RSA
private-key operations need their own review; this rationale does not cover
such use, another driver version or an unverified dependency graph.

Exit condition: remove the exception when the locked driver stops depending
on affected rsa, or adopts a patched release. Any new private-key callpath
invalidates this rationale. Record the new review before release instead of
broadening the ignore list. See [TLS configuration](BACKEND_TLS.md).
