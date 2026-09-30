---
title: Typed field constants
description: Why QuerySet filters use Field<M, T> constants instead of a filter! macro or Django-style lookups.
---

```rust
User::objects(&db)
    .filter(User::name.icontains("john").or(User::email.ends_with("@example.com")))
    .order_by([User::name.asc()])
```

`#[derive(Model)]` generates one associated constant per field:
`pub const name: Field<User, String>`. The type parameters encode two
things:

- The **model**, so a `Field<Post, _>` cannot be used by mistake in a
  `User` query.
- The **Rust type**. Lookups are inherent methods that exist only where
  they make sense. `icontains` exists only on `Field<M, String>`, and
  comparison operands must implement `Operand<M, T>`. That means
  `User::age.eq("x")` fails to **compile**.

A `filter!(User, name__icontains = "john")` macro was considered and
rejected. The typed-constant API gives IDE completion, rustdoc, and
ordinary compiler errors. A macro would still have to generate these same
constants in order to validate field names.

Django `__` traversal is `Post::author.join(Author::name)` (and further
hops on the joined handle). There are no keyword arguments in Rust, so
`name__icontains=` is not possible.

Implemented in `siderite_orm::expr::{Expr, Field, Operand, Lookup}`.

## See also

- [QuerySets](/siderite/guides/data/querysets/)
- [Architecture](/siderite/internals/architecture/)
- [QueryPlan IR](/siderite/internals/query-plan/)
