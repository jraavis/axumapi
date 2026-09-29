//! Recognition of hook methods and their attribute arguments.

use crate::diag::Errors;
use crate::docs;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, FnArg, Ident, ImplItemFn, LitStr, ReturnType, Token, Type};

/// Names of the helper attributes.
pub const HELPERS: &[&str] = &[
    "field_validator",
    "model_validator",
    "computed_field",
    "field_serializer",
    "model_serializer",
];

/// When a validator runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On the raw input, before coercion of the field/model.
    Before,
    /// On the typed value, after deserialization.
    After,
}

/// A recognised hook method, in declaration order.
pub enum Hook {
    FieldValidator {
        mode: Mode,
        fields: Vec<LitStr>,
        method: Ident,
    },
    ModelValidator {
        mode: Mode,
        method: Ident,
    },
    Computed {
        key: String,
        ty: Box<Type>,
        method: Ident,
        doc: Option<String>,
    },
    FieldSerializer {
        field: LitStr,
        method: Ident,
    },
    ModelSerializer {
        method: Ident,
    },
}

enum Arg {
    Field(LitStr),
    Named(Ident, LitStr),
}

impl Parse for Arg {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(Ident) {
            let name: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            Ok(Self::Named(name, input.parse()?))
        } else {
            Ok(Self::Field(input.parse()?))
        }
    }
}

/// Arguments of a helper attribute: string fields plus `name = "value"`.
struct Args {
    fields: Vec<LitStr>,
    named: Vec<(Ident, LitStr)>,
}

fn parse_args(attr: &Attribute) -> syn::Result<Args> {
    let mut out = Args {
        fields: Vec::new(),
        named: Vec::new(),
    };
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok(out);
    }
    for arg in attr.parse_args_with(Punctuated::<Arg, Token![,]>::parse_terminated)? {
        match arg {
            Arg::Field(lit) => out.fields.push(lit),
            Arg::Named(name, lit) => out.named.push((name, lit)),
        }
    }
    Ok(out)
}

fn mode(args: &Args, errors: &mut Errors) -> Mode {
    let mut mode = Mode::After;
    for (name, lit) in &args.named {
        if name == "mode" {
            match lit.value().as_str() {
                "after" => mode = Mode::After,
                "before" => mode = Mode::Before,
                other => errors.spanned(
                    lit,
                    format!("unknown mode `{other}`; expected \"after\" or \"before\""),
                ),
            }
        } else {
            errors.spanned(name, format!("unknown argument `{name}`; expected `mode`"));
        }
    }
    mode
}

fn only_named(args: &Args, allowed: &str, errors: &mut Errors) {
    for (name, _) in &args.named {
        if name != allowed {
            errors.spanned(
                name,
                format!("unknown argument `{name}`; expected `{allowed}`"),
            );
        }
    }
}

fn no_fields(args: &Args, what: &str, errors: &mut Errors) {
    for field in &args.fields {
        errors.spanned(field, format!("`{what}` does not take field names"));
    }
}

/// Number of non-receiver parameters and whether there is a receiver.
fn shape(method: &ImplItemFn) -> (bool, usize) {
    let receiver = method
        .sig
        .inputs
        .iter()
        .any(|arg| matches!(arg, FnArg::Receiver(_)));
    let params = method.sig.inputs.len() - usize::from(receiver);
    (receiver, params)
}

fn expect_shape(
    method: &ImplItemFn,
    receiver: bool,
    params: usize,
    expected: &str,
    errors: &mut Errors,
) {
    if shape(method) != (receiver, params) {
        errors.spanned(
            &method.sig,
            format!("this hook must have the signature `{expected}`"),
        );
    }
}

/// Recognise the hook attribute of `method`, if any.
pub fn recognise(method: &ImplItemFn, errors: &mut Errors) -> Option<Hook> {
    let helpers: Vec<&Attribute> = method
        .attrs
        .iter()
        .filter(|a| HELPERS.iter().any(|h| a.path().is_ident(h)))
        .collect();
    let [attr] = helpers.as_slice() else {
        if helpers.len() > 1 {
            errors.spanned(helpers[1], "a method can carry only one hook attribute");
        }
        return None;
    };
    let args = errors.absorb(parse_args(attr))?;
    let name = method.sig.ident.clone();
    let attr_name = attr.path().get_ident().map(ToString::to_string)?;
    match attr_name.as_str() {
        "field_validator" => {
            only_named(&args, "mode", errors);
            let mode = mode(&args, errors);
            if args.fields.is_empty() {
                errors.spanned(attr, "`field_validator` needs at least one field name");
            }
            match mode {
                Mode::After => expect_shape(
                    method,
                    false,
                    1,
                    "fn(value: &FieldType) -> Result<(), FieldError>",
                    errors,
                ),
                Mode::Before => expect_shape(
                    method,
                    false,
                    1,
                    "fn(value: &mut Value) -> Result<(), FieldError>",
                    errors,
                ),
            }
            Some(Hook::FieldValidator {
                mode,
                fields: args.fields,
                method: name,
            })
        }
        "model_validator" => {
            no_fields(&args, "model_validator", errors);
            only_named(&args, "mode", errors);
            let mode = mode(&args, errors);
            match mode {
                Mode::After => {
                    expect_shape(
                        method,
                        true,
                        0,
                        "fn(&self) -> Result<(), FieldError>",
                        errors,
                    );
                }
                Mode::Before => expect_shape(
                    method,
                    false,
                    1,
                    "fn(input: &mut Value) -> Result<(), FieldError>",
                    errors,
                ),
            }
            Some(Hook::ModelValidator { mode, method: name })
        }
        "computed_field" => {
            no_fields(&args, "computed_field", errors);
            only_named(&args, "alias", errors);
            expect_shape(method, true, 0, "fn(&self) -> T", errors);
            let ty = match &method.sig.output {
                ReturnType::Type(_, ty) if !matches!(**ty, Type::ImplTrait(_)) => ty.clone(),
                other => {
                    errors.spanned(
                        other,
                        "computed fields must return a nameable type (not `impl Trait`) \
                         so the schema can describe it",
                    );
                    return None;
                }
            };
            let key = args.named.iter().find(|(n, _)| n == "alias").map_or_else(
                || syn::ext::IdentExt::unraw(&name).to_string(),
                |(_, lit)| lit.value(),
            );
            Some(Hook::Computed {
                key,
                ty,
                method: name,
                doc: docs::description(&method.attrs),
            })
        }
        "field_serializer" => {
            only_named(&args, "", errors);
            expect_shape(
                method,
                false,
                1,
                "fn(value: &FieldType) -> impl Serialize",
                errors,
            );
            let mut fields = args.fields.into_iter();
            let (Some(field), None) = (fields.next(), fields.next()) else {
                errors.spanned(attr, "`field_serializer` takes exactly one field name");
                return None;
            };
            Some(Hook::FieldSerializer {
                field,
                method: name,
            })
        }
        _ => {
            no_fields(&args, "model_serializer", errors);
            only_named(&args, "", errors);
            expect_shape(method, true, 1, "fn(&self, value: Value) -> Value", errors);
            Some(Hook::ModelSerializer { method: name })
        }
    }
}
