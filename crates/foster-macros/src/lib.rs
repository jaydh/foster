extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::{Delimiter, TokenTree};

/// Validate a state machine graph at compile time and generate typed `State`/`Event` enums.
///
/// # Syntax
/// ```rust,ignore
/// machine_graph! {
///     id: "counter",
///     initial: "idle",
///     states: ["idle", "error"],
///     transitions: [
///         ("idle", "increment", "idle"),
///         ("idle", "break_it",  "error"),
///         ("error", "recover",  "idle"),
///     ]
/// }
/// ```
///
/// Emits compile errors for:
/// - `initial` state not in `states`
/// - Transition source or target state not in `states`
/// - States unreachable from `initial` via any transition path
///
/// On success, generates (for `id = "counter"`):
/// - `enum CounterState { Idle, Error }` with `fn as_str(self) -> &'static str`
/// - `enum CounterEvent { Increment, BreakIt, Recover }` with `fn as_str(self) -> &'static str`
///
/// Events are collected from the transitions list in encounter order, deduplicated.
#[proc_macro]
pub fn machine_graph(input: TokenStream) -> TokenStream {
    match mg_parse_and_generate(input) {
        Ok(ts) => ts,
        Err(msg) => {
            let lit = proc_macro2::Literal::string(&msg);
            quote::quote!(compile_error!(#lit);).into()
        }
    }
}

fn mg_parse_and_generate(input: TokenStream) -> Result<TokenStream, String> {
    let tokens: Vec<TokenTree> = proc_macro2::TokenStream::from(input).into_iter().collect();

    let mut id: Option<String> = None;
    let mut initial: Option<String> = None;
    let mut states: Vec<String> = Vec::new();
    let mut transitions: Vec<(String, String, String)> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        if let TokenTree::Punct(p) = &tokens[i] {
            if p.as_char() == ',' {
                i += 1;
                continue;
            }
        }
        if i + 2 >= tokens.len() {
            break;
        }
        let key = match &tokens[i] {
            TokenTree::Ident(ident) => ident.to_string(),
            _ => { i += 1; continue; }
        };
        match &tokens[i + 1] {
            TokenTree::Punct(p) if p.as_char() == ':' => {}
            _ => return Err(format!("machine_graph!: expected ':' after key '{}'", key)),
        }
        let value = &tokens[i + 2];
        match key.as_str() {
            "id"      => { id      = Some(mg_extract_str(value)?); i += 3; }
            "initial" => { initial = Some(mg_extract_str(value)?); i += 3; }
            "states"  => { states  = mg_parse_str_array(value)?;   i += 3; }
            "transitions" => { transitions = mg_parse_transitions(value)?; i += 3; }
            _ => return Err(format!(
                "machine_graph!: unknown key '{}' — expected id, initial, states, or transitions",
                key
            )),
        }
    }

    let id      = id.ok_or("machine_graph!: missing required field 'id'")?;
    let initial = initial.ok_or("machine_graph!: missing required field 'initial'")?;

    if states.is_empty() {
        return Err("machine_graph!: 'states' list must not be empty".into());
    }

    let states_set: std::collections::HashSet<&str> =
        states.iter().map(|s| s.as_str()).collect();

    if !states_set.contains(initial.as_str()) {
        return Err(format!(
            "machine_graph!: initial state '{}' is not in states list {:?}",
            initial, states
        ));
    }

    for (from, event, to) in &transitions {
        if !states_set.contains(from.as_str()) {
            return Err(format!(
                "machine_graph!: transition source '{}' (event '{}') is not a declared state. \
                 Declared: {:?}",
                from, event, states
            ));
        }
        if !states_set.contains(to.as_str()) {
            return Err(format!(
                "machine_graph!: transition target '{}' (from '{}', event '{}') is not a declared state. \
                 Declared: {:?}",
                to, from, event, states
            ));
        }
    }

    // BFS reachability from initial
    let mut reachable: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    queue.push_back(initial.as_str());
    while let Some(s) = queue.pop_front() {
        if !reachable.insert(s) { continue; }
        for (from, _, to) in &transitions {
            if from.as_str() == s {
                queue.push_back(to.as_str());
            }
        }
    }
    let unreachable: Vec<&str> = states.iter()
        .filter(|s| !reachable.contains(s.as_str()))
        .map(|s| s.as_str())
        .collect();
    if !unreachable.is_empty() {
        return Err(format!(
            "machine_graph!: unreachable states [{}] — every state must be reachable from '{}'",
            unreachable.join(", "), initial
        ));
    }

    mg_generate_types(&id, &states, &transitions)
}

fn mg_generate_types(
    id: &str,
    states: &[String],
    transitions: &[(String, String, String)],
) -> Result<TokenStream, String> {
    use proc_macro2::{Ident, Span};
    use quote::quote;

    let span = Span::call_site();
    let prefix = mg_to_pascal_case(id);

    let state_enum = Ident::new(&format!("{}State", prefix), span);
    let event_enum = Ident::new(&format!("{}Event", prefix), span);

    let state_variants: Vec<Ident> = states.iter()
        .map(|s| Ident::new(&mg_to_pascal_case(s), span))
        .collect();
    let state_str_arms: Vec<_> = states.iter().zip(&state_variants).map(|(s, v)| {
        let lit = proc_macro2::Literal::string(s);
        quote! { Self::#v => #lit, }
    }).collect();

    // Events: unique, in encounter order
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut events: Vec<String> = Vec::new();
    for (_, ev, _) in transitions {
        if seen.insert(ev.clone()) { events.push(ev.clone()); }
    }
    let event_variants: Vec<Ident> = events.iter()
        .map(|e| Ident::new(&mg_to_pascal_case(e), span))
        .collect();
    let event_str_arms: Vec<_> = events.iter().zip(&event_variants).map(|(e, v)| {
        let lit = proc_macro2::Literal::string(e);
        quote! { Self::#v => #lit, }
    }).collect();

    let out = quote! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum #state_enum {
            #(#state_variants,)*
        }
        impl #state_enum {
            /// Returns the machine state name as used in the wire protocol and HTML attributes.
            pub fn as_str(self) -> &'static str {
                match self { #(#state_str_arms)* }
            }
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum #event_enum {
            #(#event_variants,)*
        }
        impl #event_enum {
            /// Returns the event name as used in the wire protocol and `fx-on` attributes.
            pub fn as_str(self) -> &'static str {
                match self { #(#event_str_arms)* }
            }
        }
    };

    Ok(out.into())
}

/// Convert a snake_case or kebab-case string to PascalCase for use as a Rust type name.
fn mg_to_pascal_case(s: &str) -> String {
    s.split(|c: char| c == '_' || c == '-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect()
}

fn mg_extract_str(token: &TokenTree) -> Result<String, String> {
    match token {
        TokenTree::Literal(lit) => {
            let s = lit.to_string();
            if s.starts_with('"') || s.starts_with('r') {
                Ok(extract_str_content(&s))
            } else {
                Err(format!("machine_graph!: expected string literal, got: {}", s))
            }
        }
        _ => Err("machine_graph!: expected a string literal".into()),
    }
}

fn mg_parse_str_array(token: &TokenTree) -> Result<Vec<String>, String> {
    let group = match token {
        TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket => g,
        _ => return Err("machine_graph!: 'states' must be a [...] array of string literals".into()),
    };
    let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
    let mut result = Vec::new();
    for t in &tokens {
        match t {
            TokenTree::Literal(_) => result.push(mg_extract_str(t)?),
            TokenTree::Punct(p) if p.as_char() == ',' => {}
            _ => {}
        }
    }
    Ok(result)
}

fn mg_parse_transitions(token: &TokenTree) -> Result<Vec<(String, String, String)>, String> {
    let group = match token {
        TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket => g,
        _ => return Err("machine_graph!: 'transitions' must be a [...] array of 3-tuples".into()),
    };
    let tokens: Vec<TokenTree> = group.stream().into_iter().collect();
    let mut result = Vec::new();
    for t in &tokens {
        if let TokenTree::Group(g) = t {
            if g.delimiter() == Delimiter::Parenthesis {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                let strs: Vec<String> = inner.iter()
                    .filter_map(|tok| {
                        if let TokenTree::Literal(_) = tok { mg_extract_str(tok).ok() } else { None }
                    })
                    .collect();
                if strs.len() != 3 {
                    return Err(format!(
                        "machine_graph!: each transition must be (\"from\", \"event\", \"to\"), \
                         got {} element(s)",
                        strs.len()
                    ));
                }
                result.push((strs[0].clone(), strs[1].clone(), strs[2].clone()));
            }
        }
    }
    Ok(result)
}

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
