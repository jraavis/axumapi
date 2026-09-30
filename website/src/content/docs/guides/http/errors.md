---
title: Errors
description: RFC 7807 problem documents, validation 422s, and how ORM errors map to HTTP.
---

Library errors render as RFC 7807 `application/problem+json`. Handlers
return `Result<T, ApiError>` (`ApiResult<T>` is an alias) or build an
`ApiError` directly.

## HTTP mapping

| Error | Crate | HTTP |
|---|---|---|
| `ValidationError` | validation | 422 with `errors` extension |
| `QueryError::DoesNotExist` | ORM | 404 |
| `QueryError::MultipleObjectsReturned` | ORM | 500 |
| `BackendCapabilityError` | ORM | 501 |
| `BackendError::Constraint` | ORM | 409 (the detail is logged, not returned) |
| `OrmError::Signal` | ORM | 500 (the text is logged, not returned) |
| `OrmError::UnknownDatabase` | ORM | 500 (a missing alias is a configuration error) |
| other `BackendError` | ORM | 500 (the detail is logged, not returned) |
| `ApiError` | core | RFC 7807 `application/problem+json` |

`core` implements `From<OrmError> for ApiError`. The ORM crate has no HTTP
knowledge.

## 422 shape

Invalid `Json<T>`, `Query<T>`, or `Form<T>` never reaches the handler:

```json
{
  "type": "about:blank",
  "title": "Unprocessable Entity",
  "status": 422,
  "detail": "The request could not be processed; see `errors` for details.",
  "errors": [
    {"location": ["body", "email"], "code": "invalid_email", "message": "invalid email address"}
  ]
}
```

See [Validation](/siderite/guides/http/validation/) for the pipeline and location
rules.

## Building an `ApiError`

Use the constructors on `ApiError` (`not_found`, `internal`, status
helpers) and, when a response must carry headers (for example
`WWW-Authenticate`), the header-carrying variants used by the security
extractors.

Capability errors mean “this backend cannot run this plan”. They are raised
before any I/O. See [Backends](/siderite/guides/data/backends/).

## What is never returned to the client

Bind parameters, database constraint text, signal messages, and `Secret`
values are logged and stripped from the HTTP body. Configuration errors
such as an unknown database alias are 500s, not client errors.

## See also

- [Security](/siderite/guides/http/security/) — 401 / 403 from extractors
- [Observability](/siderite/guides/production/observability/) — what spans record
- [Architecture](/siderite/internals/architecture/) — error architecture
