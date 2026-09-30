//! Documentation UI pages. Assets load from jsDelivr; the pages embed only
//! the (escaped) spec URL and title.

/// Minimal HTML escaping for attribute and text contexts.
fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

/// Swagger UI page loading the document at `spec_url`.
pub fn swagger_ui_html(title: &str, spec_url: &str) -> String {
    let (title, url) = (escape(title), escape(spec_url));
    format!(
        r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} – Swagger UI</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css">
</head><body><div id="swagger-ui" data-url="{url}"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>
const el = document.getElementById("swagger-ui");
SwaggerUIBundle({{ url: el.dataset.url, dom_id: "#swagger-ui" }});
</script></body></html>"##
    )
}

/// ReDoc page loading the document at `spec_url`.
pub fn redoc_html(title: &str, spec_url: &str) -> String {
    let (title, url) = (escape(title), escape(spec_url));
    format!(
        r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} – ReDoc</title></head>
<body><redoc spec-url="{url}"></redoc>
<script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"></script>
</body></html>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_escape_inputs() {
        let html = swagger_ui_html("<x>", "/openapi.json?a=\"b\"");
        assert!(html.contains("&lt;x&gt;"));
        assert!(html.contains("data-url=\"/openapi.json?a=&quot;b&quot;\""));
        assert!(redoc_html("t", "/o").contains("spec-url=\"/o\""));
    }
}
