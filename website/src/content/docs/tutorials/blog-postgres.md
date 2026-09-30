---
title: Blog on PostgreSQL
description: AppCli, migrations, OAuth2 password flow, signals, and pagination.
---

Package: `examples/blog_postgres`. A complete blog API: users, posts with
tags, comments, OAuth2 password-flow authentication, pagination, OpenAPI
tags, and an audit trail written by model signal receivers.

## Run

```bash
export DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite
cargo run -p blog_postgres -- migrate
ADDR=127.0.0.1:18080 cargo run -p blog_postgres -- runserver
```

Start PostgreSQL from the root `docker-compose.yml` if you do not already
have one. Other commands: `check`, `routes`, `showmigrations`, `rollback`,
`dbshell`, and `makemigrations` (regenerate after changing `src/models.rs`).
Interactive docs: `/docs`, `/openapi.json`.

Tests (`cargo test -p blog_postgres`) run the API end to end against
in-memory SQLite, applying the same generated migrations that PostgreSQL
uses.

## Try it

```bash
B=http://127.0.0.1:18080
curl -XPOST $B/users -H 'content-type: application/json' \
  -d '{"username":"ann","password":"secret-pass"}'
TOKEN=$(curl -s -XPOST $B/token -d 'username=ann&password=secret-pass' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["access_token"])')
curl -XPOST $B/posts -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"title":"Hello","body":"First post","published":true,"tags":["rust"]}'
curl "$B/posts?tag=rust&page=1&per_page=10"
```

## What it shows

| Piece | Where |
|---|---|
| Models `User`, `Post` (FK author, M2M tags), `Tag`, `Comment`, `AccessToken`, `AuditEntry` | `src/models.rs` |
| `POST /token`, `Security<CurrentUser, WritePosts>` with scopes `posts:write`, `comments:write` | `src/auth.rs` |
| Pagination (`?page=&per_page=`, envelope with `total`, `total_pages`) | `src/pagination.rs` |
| `post_save` / `post_delete` receivers writing `audit_log` in the caller’s transaction | `src/receivers.rs` |
| Routes tagged `users`, `auth`, `posts`, `comments`, `tags` | `src/{users,posts,comments}.rs` |

The binary is an `AppCli`: `configure_db` attaches the signal registry once
per connected alias. Tokens are opaque random values stored in
`access_tokens` and expire after one hour. Drafts are visible only to their
author; only the author may edit or delete a post.

## Security note

`hash_password` in `src/auth.rs` is a **placeholder (salted, iterated
FNV-1a) and is not production password hashing.** Use Argon2id, scrypt, or
bcrypt in a real application, and compare secrets in constant time.

## See also

- [CLI](/siderite/guides/production/cli/)
- [Security](/siderite/guides/http/security/)
- [Signals](/siderite/guides/data/signals/)
- [Migrations](/siderite/guides/data/migrations/)
