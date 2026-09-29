# QueryPlan IR

`axumapi_orm::QueryPlan` is the single backend-neutral representation that every high-level query compiles into.

* **Immutable builder.** Each method takes and returns `self`. `clone()` a base plan to branch safely.
* **Pure data.** Building a plan never performs I/O.
* **Capability-aware.** `required_features()` walks the whole tree, including subqueries. It reports joins, row locks (and their modifiers), regex lookups and `DISTINCT ON`. `check(&caps)` turns any mismatch into a typed `BackendCapabilityError`.

| Field | Meaning | Relational-only? |
|---|---|---|
| `source` | table / collection | no |
| `projection` | selected expressions (empty = all columns) | no |
| `joins` | joined sources | yes → `Feature::Joins` |
| `filter` / `having` | `Expr` predicates | no |
| `grouping` | group keys | no (maps to `$group`) |
| `ordering`, `limit`, `offset` | sorting and paging | no |
| `distinct` | `None` / `All` / `On(..)` | `On` → `Feature::DistinctOn` |
| `lock` | `FOR UPDATE` variants | yes → `Feature::RowLocking` |

The `Expr` nodes are `Column` (F), `Value` (always bound), `Binary` (comparison and arithmetic), `Unary`, `And`/`Or` (Q, flattened), `Lookup` (iexact, contains, startswith, endswith, regex, in, range, isnull), `Exists` and `Subquery`.

Planned for Phases 3–4: function calls (lower, coalesce, …), `Case`/`When`, aggregates, window functions, date-part lookups and outer references.
