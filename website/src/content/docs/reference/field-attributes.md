---
title: Field attributes
description: Combined #[field] keys for Validate, Schema, and Model.
---

`#[field(...)]` is shared. Each derive reads the keys it understands.

- `Validate` and `Schema` ignore ORM keys.
- `Model` ignores validation keys except `max_length`, `max_digits`, and
  `decimal_places`, which become column metadata.
- Unknown keys are compile errors on `Model`. `Schema` ignores unknown
  keys so other derives can share the attribute.

Key names for JSON always come from Serde (`rename`, `rename_all`).

## Validation and schema

| Key | Validation | Schema |
|---|---|---|
| `min_length`, `max_length` | Characters or items | `minLength` / `maxLength` or `minItems` / `maxItems` |
| `pattern = "regex"` | Whole-value match; regex checked at expand time | `pattern` |
| `email` | Email format | `format: email` |
| `url` | — | `format: uri` (with `#[derive(Schema)]`) |
| `gt`, `ge`, `lt`, `le` | Numeric bounds | `exclusiveMinimum`, `minimum`, `exclusiveMaximum`, `maximum` |
| `multiple_of` | Divisibility | `multipleOf` |
| `validation_alias = "x"` | Extra accepted input key | — |
| `alias` | — | Schema annotation |
| `default = expr`, `default_factory = path` | Field optional; Serde must also default it | Left out of `required` |
| `strict` | Strict mode for this field | — |
| `exclude` | Left out of `dump` | — |
| `validator = path` | Reusable after-validator | — |
| `title`, `description`, `examples(...)` | — | Annotations |
| `max_digits`, `decimal_places` | Documented as `x-` extensions unless the type is `Decimal<D, P>` | column metadata for `Model` |

## ORM

| Key | Meaning |
|---|---|
| `primary_key` | Exactly one field |
| `auto` | Database-generated integer primary key |
| `unique`, `index` | Constraint / single-column index |
| `column = "name"` | Column name; default `{field}_id` for relations |
| `db_default = …` | Server-side default |
| `auto_now_add`, `auto_now` | `DbDefault::Now` |
| `on_delete = "cascade" \| "protect" \| "set_null" \| "set_default" \| "do_nothing"` | Relations only |
| `related_name = ".."` | Reverse accessor |
| `skip` | Not a column |

## Model-level

See [Models](/axumapi/guides/data/models/) for `#[model(...)]` and
[Validation](/axumapi/guides/http/validation/) for `#[model_config(...)]` and
`#[model_hooks]`.

## See also

- [Validation](/axumapi/guides/http/validation/)
- [Models](/axumapi/guides/data/models/)
- [Pydantic v2 mapping](/axumapi/reference/pydantic/)
