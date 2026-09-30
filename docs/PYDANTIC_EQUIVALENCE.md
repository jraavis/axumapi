# Pydantic v2 → siderite

This table maps Pydantic v2 features to their siderite equivalents. Where the behaviour differs, the difference is stated. Nothing here is claimed to be an exact match.

| Pydantic v2 | siderite | Notes |
|---|---|---|
| `class M(BaseModel)` | `#[derive(Deserialize, Validate, Schema)] struct M` | Add `Serialize` for response models. |
| `model_validate(data)` | `validation::parse_value::<M>(value, ValidationContext::new())` | Returns every error at once. |
| `model_validate(data, strict=True)` | `ValidationContext::new().with_strict(true)` | |
| `model_validate(data, context=...)` | `ValidationContext::with_data(value)`, read with `ctx.data::<T>()` | Data is keyed by its Rust type, not stored in a dict. |
| `model_validate_json(text)` | `parse_json::<M>(text, ctx)` | |
| `model_dump()` | `value.dump(&DumpOptions::new())` | Built on Serde output. |
| `model_dump(exclude_none=True)` | `DumpOptions::new().exclude_none()` | |
| `model_dump(include=..., exclude=...)` | `FieldSet` trees | A nested selection applies to every element of a list. Pydantic's index-keyed selection is not supported. |
| `model_dump(exclude_defaults=True)` | `exclude_defaults()` | Only fields that declare `#[field(default)]`. |
| `model_dump(exclude_unset=True)` | **Deferred** | Serde does not record which fields were set. |
| `model_dump(by_alias=True)` | Always on | Output keys are Serde keys. |
| `model_json_schema()` | `schema_for::<M>()` / `App::openapi()` | JSON Schema 2020-12, with named models under `$ref` components. |
| `ConfigDict(strict=True)` | `#[model_config(strict)]` | |
| `extra='forbid' / 'ignore' / 'allow'` | `extra = "forbid" / "ignore" / "allow"` | With `allow`, extra keys are accepted and then **dropped**, because a Rust struct has nowhere to keep them. |
| `populate_by_name=True` | `populate_by_name` | |
| `str_strip_whitespace`, `str_to_lower`, `str_to_upper` | Same names | |
| `arbitrary_types_allowed` | Not needed | Any type works once it implements `Validate`. `impl Validate for T {}` performs no checks. |
| `frozen=True` | Rust immutability | Omit `mut` and setters. Nothing is intercepted at runtime. |
| `Field(min_length=, max_length=, pattern=, gt=, ge=, lt=, le=, multiple_of=)` | `#[field(...)]` with the same names | Regex patterns are checked at compile time. |
| `Field(alias=)` | `#[serde(rename)]` | Serde is the single source of truth for key names. |
| `Field(validation_alias=)` | `#[field(validation_alias)]` | |
| `Field(serialization_alias=)` | Must equal the Serde key | Each type has one schema, so different input and output names are rejected. |
| `Field(default=, default_factory=)` | `#[field(default...)]` plus `#[serde(default...)]` | Serde performs the defaulting. |
| `Field(exclude=True)` | `#[field(exclude)]` | |
| `Field(strict=True)` | `#[field(strict)]` | |
| `@field_validator('f', mode='after')` | `#[field_validator("f", mode = "after")]` in `#[model_hooks] impl` | Refers to `self.f` directly, so a misspelled field name is a compile error. |
| `@field_validator(mode='before')` | `mode = "before"`, operating on the raw `serde_json::Value` | |
| `@model_validator(mode='after' / 'before')` | `#[model_validator(mode = ...)]` | |
| `mode='wrap'`, `mode='plain'` | **Deferred** | |
| `@computed_field` | `#[computed_field]` | Documented as a `readOnly` schema property. |
| `@field_serializer`, `@model_serializer` | Same names | They run on the Serde output. |
| `Optional[int]` with no default | `Option<i32>` | **Differs:** Pydantic v2 requires the key to be present (its value may be null). siderite follows Serde, so a missing key becomes `None`. |
| Lax coercion (`"1"` → `1`) | Same in lax mode | JSON strings are never coerced **from** numbers, matching Pydantic. |
| `EmailStr`, `SecretStr`, `conint`, `constr` | `Email`, `SecretString`, `BoundedI64<..>`, `ConstrainedString<..>` | These are newtypes, not factory functions. |
| `HttpUrl`, `AnyUrl`, `IPvAnyAddress`, `UUID4`, `condecimal`, `confloat`, `conlist` | `HttpUrl`, `Url`, `IpAddress`, `Uuid`, `Decimal<D, P>`, `BoundedFloat<B>`, `ConstrainedVec<T, MIN, MAX>` | Float bounds are given as a bounds type, because stable Rust has no `f64` const generics. |
| Error `loc`, `type`, `msg` | `location`, `code`, `message` | The codes follow Pydantic's names where one exists, for example `int_parsing`, `missing`, `extra_forbidden`. |
| MessagePack serialization | **Deferred** | |
