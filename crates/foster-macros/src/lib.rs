extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenTree};

/// Compact HTML template DSL with `fx-*` shorthand attributes.
///
/// # Syntax
/// ```text
/// html! {
///     tag[attr="val", bool_attr] { children }
///     "text content"
/// }
/// ```
///
/// # Attribute shorthands
/// | Macro        | HTML attribute    |
/// |--------------|-------------------|
/// | `machine`    | `fx-machine`      |
/// | `show`       | `fx-show`         |
/// | `text`       | `fx-text`         |
/// | `on`         | `fx-on`           |
/// | `each`       | `fx-for`          |
/// | `filter`     | `fx-where`        |
/// | `collect`    | `fx-collect`      |
/// | `disable`    | `fx-disable`      |
/// | `value`      | `fx-value`        |
/// | `payload`    | `fx-payload`      |
/// | `field`      | `fx-field`        |
/// | `bind`       | `fx-bind-attr`    |
/// | `state_label`| `fx-state-label`  |
/// | `foo_bar`    | `foo-bar`         |
///
/// Other attributes are passed through with underscores converted to hyphens.
#[proc_macro]
pub fn html(input: TokenStream) -> TokenStream {
    let tokens: Vec<TokenTree> = proc_macro2::TokenStream::from(input).into_iter().collect();
    let html = render_nodes(&tokens);
    let lit = proc_macro2::Literal::string(&html);
    quote::quote!(#lit).into()
}

fn render_nodes(tokens: &[TokenTree]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let consumed = render_node(tokens, i, &mut out);
        i += consumed.max(1);
    }
    out
}

fn render_node(tokens: &[TokenTree], i: usize, out: &mut String) -> usize {
    match &tokens[i] {
        TokenTree::Ident(_) => render_element(tokens, i, out),
        TokenTree::Literal(lit) => {
            let s = lit.to_string();
            if s.starts_with('"') || s.starts_with('r') {
                let text = extract_str_content(&s);
                out.push_str(&escape_text(&text));
            }
            1
        }
        _ => 1,
    }
}

fn render_element(tokens: &[TokenTree], start: usize, out: &mut String) -> usize {
    let raw_tag = tokens[start].to_string();
    let tag = raw_tag.trim_start_matches("r#");
    let mut i = start + 1;

    let mut attrs: Vec<(String, Option<String>)> = Vec::new();
    if i < tokens.len() {
        if let TokenTree::Group(g) = &tokens[i] {
            if g.delimiter() == Delimiter::Bracket {
                attrs = parse_attrs(g.stream());
                i += 1;
            }
        }
    }

    let body = if i < tokens.len() {
        if let TokenTree::Group(g) = &tokens[i] {
            if g.delimiter() == Delimiter::Brace {
                let child_tokens: Vec<_> = g.stream().into_iter().collect();
                let content = render_nodes(&child_tokens);
                i += 1;
                Some(content)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    out.push('<');
    out.push_str(tag);
    for (name, value) in &attrs {
        out.push(' ');
        out.push_str(name);
        if let Some(v) = value {
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
    }

    if is_void(tag) {
        out.push('>');
    } else if let Some(content) = body {
        out.push('>');
        out.push_str(&content);
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    } else {
        out.push_str("></");
        out.push_str(tag);
        out.push('>');
    }

    i - start
}

fn parse_attrs(stream: proc_macro2::TokenStream) -> Vec<(String, Option<String>)> {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut attrs = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            TokenTree::Ident(ident) => {
                let raw = ident.to_string();
                let raw = raw.trim_start_matches("r#");

                // Check for `name = "value"` form
                let has_eq = i + 1 < tokens.len()
                    && matches!(&tokens[i + 1], TokenTree::Punct(p) if p.as_char() == '=');
                let has_val = has_eq
                    && i + 2 < tokens.len()
                    && matches!(&tokens[i + 2], TokenTree::Literal(_));

                if has_val {
                    if let TokenTree::Literal(lit) = &tokens[i + 2] {
                        let val = escape_attr(&extract_str_content(&lit.to_string()));
                        attrs.push((map_attr(raw), Some(val)));
                        i += 3;
                        skip_comma(&tokens, &mut i);
                        continue;
                    }
                }

                // Boolean attribute (no value)
                attrs.push((map_attr(raw), None));
                i += 1;
                skip_comma(&tokens, &mut i);
            }
            TokenTree::Punct(p) if p.as_char() == ',' => {
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    attrs
}

fn skip_comma(tokens: &[TokenTree], i: &mut usize) {
    if *i < tokens.len() {
        if let TokenTree::Punct(p) = &tokens[*i] {
            if p.as_char() == ',' {
                *i += 1;
            }
        }
    }
}

fn map_attr(name: &str) -> String {
    match name {
        "machine" => "fx-machine".into(),
        "show" => "fx-show".into(),
        "text" => "fx-text".into(),
        "on" => "fx-on".into(),
        "each" => "fx-for".into(),
        "filter" => "fx-where".into(),
        "collect" => "fx-collect".into(),
        "disable" => "fx-disable".into(),
        "value" => "fx-value".into(),
        "payload" => "fx-payload".into(),
        "field" => "fx-field".into(),
        "bind" => "fx-bind-attr".into(),
        "state_label" => "fx-state-label".into(),
        other => other.replace('_', "-"),
    }
}

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input"
            | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

/// Extract string content from a Rust string literal token (`"..."` or `r#"..."#`).
fn extract_str_content(s: &str) -> String {
    if s.starts_with('r') {
        let hashes = s[1..].chars().take_while(|c| *c == '#').count();
        let start = 1 + hashes + 1;
        let end = s.len() - 1 - hashes;
        s[start..end].to_string()
    } else if s.starts_with('"') {
        decode_escapes(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

fn decode_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some('u') => {
                if chars.next() == Some('{') {
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(n) {
                            out.push(ch);
                        }
                    }
                }
            }
            Some(c) => { out.push('\\'); out.push(c); }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
