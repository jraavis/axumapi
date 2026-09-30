//! Compile-time validation of route path templates.

use crate::diag::Errors;
use std::collections::HashSet;
use syn::LitStr;

/// Report every problem found in the path template `lit`.
pub fn validate(lit: &LitStr, errors: &mut Errors) {
    for problem in problems(&lit.value()) {
        errors.error(lit.span(), problem);
    }
}

fn problems(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !path.starts_with('/') {
        out.push(format!("route path must start with '/', found `{path}`"));
    }
    let bytes = path.as_bytes();
    let mut seen = HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                let close = path[i + 1..].find(['{', '}']);
                match close {
                    Some(off) if bytes[i + 1 + off] == b'}' => {
                        check_param(&path[i + 1..i + 1 + off], &mut seen, &mut out);
                        i += off + 2;
                        continue;
                    }
                    _ => {
                        out.push(format!("unbalanced `{{` at byte {i} of path `{path}`"));
                        break;
                    }
                }
            }
            b'}' => out.push(format!("unbalanced `}}` at byte {i} of path `{path}`")),
            _ => {}
        }
        i += 1;
    }
    out
}

fn check_param(raw: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    // `{*rest}` is a catch-all parameter.
    let name = raw.strip_prefix('*').unwrap_or(raw);
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        out.push(format!(
            "invalid path parameter name `{{{raw}}}`: expected an identifier such as `{{id}}`"
        ));
    } else if !seen.insert(name.to_owned()) {
        out.push(format!("duplicate path parameter `{name}`"));
    }
}
