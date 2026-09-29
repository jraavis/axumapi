//! Doc comment extraction.

use syn::{Attribute, Expr, ExprLit, Lit, Meta};

/// Trimmed lines of the doc comment attached to `attrs`, blank edges removed.
pub fn lines(attrs: &[Attribute]) -> Vec<String> {
    let mut lines: Vec<String> = attrs
        .iter()
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(nv) if nv.path.is_ident("doc") => match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .flat_map(|doc| {
            doc.lines()
                .map(|line| line.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    trim_blank_edges(&mut lines);
    lines
}

fn trim_blank_edges(lines: &mut Vec<String>) {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let leading = lines.iter().take_while(|l| l.is_empty()).count();
    lines.drain(..leading);
}

/// The whole doc comment as one string, if there is one.
pub fn description(attrs: &[Attribute]) -> Option<String> {
    let lines = lines(attrs);
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// First doc line as a summary, the remaining lines as a description.
pub fn summary_and_description(attrs: &[Attribute]) -> (Option<String>, Option<String>) {
    let mut all = lines(attrs);
    if all.is_empty() {
        return (None, None);
    }
    let summary = all.remove(0);
    trim_blank_edges(&mut all);
    let description = (!all.is_empty()).then(|| all.join("\n"));
    (Some(summary), description)
}
