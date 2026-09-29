# QueryPlan IR

`axumapi_orm::QueryPlan` is the single backend-neutral representation that every high-level query compiles into.

* **Immutable builder.** Each method takes and returns `self`. `clone()` a base plan to branch safely.
* **Pure data.** Building a plan never performs I/O.
* **Capability-aware.** `required_features()` walks the whole tree, including subqueries, derived tables and set-operation members. `check(&caps)` turns any mismatch into a typed `BackendCapabilityError`. Backends check *before* any I/O, and multi-statement terminals (`paginate`, `get_or_create`, the bulk operations) check first as well.

| Field | Meaning | Relational-only? |
|---|---|---|
| `source` | table, or a derived table `(plan) AS alias` | no |
| `projection` | selected expressions (empty = all columns of the root source) | no |
| `joins` | joined sources | yes → `Feature::Joins` |
| `filter` / `having` | `Expr` predicates | no |
| `grouping` | group keys | no (maps to `$group`) |
| `ordering`, `limit`, `offset` | sorting and paging | no |
| `distinct` | `None` / `All` / `On(..)` | `On` → `Feature::DistinctOn` |
| `lock` | `FOR UPDATE` variants | yes → `Feature::RowLocking`, `LockModifiers` |
| `compound` | `UNION` / `UNION ALL` / `INTERSECT` / `EXCEPT` members | no |

For a plan with `compound` members, `ordering`, `limit` and `offset` apply to the combined result. Every member must project the same columns, and a member with its own ordering or limit is wrapped in a derived table (`SELECT * FROM (..) AS "u1"`), because SQLite rejects bare parenthesised members.

## Expressions

| `Expr` node | Meaning | Feature |
|---|---|---|
| `Column`, `Value` | column reference (Django `F`), bound literal | |
| `Binary`, `Unary`, `And`, `Or` | comparison, arithmetic, boolean logic (Django `Q`, flattened) | |
| `Lookup` | iexact, contains, startswith, endswith, regex, in, in-subquery, range, isnull | `Regex` |
| `Func` | `lower`, `upper`, `length`, `coalesce`, `concat`, `substr`, `replace`, `trim` | |
| `Cast` | `CAST(x AS type)` with dialect type names | |
| `Case` | searched `CASE WHEN .. THEN .. ELSE .. END` | |
| `DatePart` | year, month, day, week (ISO), quarter, hour, minute, second, date | |
| `Aggregate` | `Count`, `Sum`, `Avg`, `Min`, `Max`, `StdDev`, `Variance`, `ArrayAgg`, `StringAgg`, with `DISTINCT` and `FILTER` | `StatisticalAggregates`, `Arrays` |
| `Window` | `RowNumber`, `Rank`, `DenseRank`, `PercentRank`, `CumeDist`, `Ntile`, `Lag`, `Lead`, `FirstValue`, `LastValue`, or an aggregate, with `PARTITION BY` / `ORDER BY` | `WindowFunctions` |
| `Exists`, `Subquery` | `EXISTS (..)`, scalar subquery | |
| `OuterRef` | column of the enclosing query, inside a correlated subquery | |
| `Related` | column reached through foreign keys, resolved into joins | `Joins` |

Comparing with a `NULL` literal (`Author::age.eq(None::<i32>)`) compiles to `IS NULL` / `IS NOT NULL`, as in Django's `filter(x=None)`.

## Compiler rules

* Values are always bound, with one exception: a `NULL` value is written as the keyword `NULL`. That keeps the statement text different from the same statement with a value, which matters because SQLx caches PostgreSQL prepared statements by text. The other literals written into SQL text are integers that PostgreSQL requires as `integer` (`LIMIT`, `OFFSET`, `NTILE(n)`, `LAG` offsets); `SUBSTR` positions are bound and cast.
* When a plan has joins, unqualified columns are qualified with the root source, and an empty projection becomes `"root".*`, so joined tables' columns never clash.
* Parameters are numbered across the whole statement, subqueries included.
* `UPDATE` and `DELETE` targets count as the enclosing query of their subqueries, so `OuterRef` works in their filters and assignments. A subquery over the *same* table as its enclosing query must name its table with `aliased(..)`; without it the compiler refuses the plan, because the inner name would shadow the outer one and turn the correlation into a tautology.
* Structural problems (wrong function arity, `OuterRef` outside a subquery, an unresolved `Related` column) are `QueryError::InvalidPlan`; they never produce broken SQL.

## From QuerySet to plan

`QuerySet` builds the plan incrementally and keeps it normalised:

* **Related traversal.** `Book::author.join(Author::name)` is a `Joined` handle whose lookups produce `Expr::Related`. `QueryPlan::resolve_relations` turns each into a `LEFT JOIN` with a stable alias (`author`, `author__team`), shared between uses. Left joins keep rows whose nullable foreign key is unset.
* **Annotations.** `annotate("n", expr)` / `alias("n", expr)` name an expression. Later `filter`, `order_by` and `project` calls refer to it with `Expr::col("n")`, and the expression is inlined, because PostgreSQL does not allow output aliases in `WHERE` or `HAVING`. A predicate containing an aggregate becomes `HAVING`. A filter on a window function is an error (SQL cannot express it without a derived table).
* **Grouping.** When the projection contains aggregates, the other projected expressions become `GROUP BY`, and the model's default ordering is dropped. Group with `project([..]).annotate(..)`.
* **`count`, `exists`, `aggregate`.** Ordering and row locks are removed (PostgreSQL rejects `ORDER BY` next to a bare aggregate). A plan with limit, offset, distinct, grouping or set operations is wrapped: `SELECT COUNT(*) FROM (..) AS "counted"`.
* **`update` and `delete`.** A plain filter is used directly; a queryset with joins, a limit or distinct becomes `WHERE pk IN (SELECT pk ..)`.

Not modelled: window frames (the database default frame applies), `GROUP BY` on expressions other than the projection, and set operations whose members are themselves grouped with different names.
