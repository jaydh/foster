//! The `fx-on` attribute grammar, shared by template validation (server) and
//! the WASM client so both read it the same way.
//!
//! `fx-on` holds one or more space-separated bindings, each `trigger->event`:
//! - `trigger` is a DOM event (`click`, `input`, `change`, `keydown`), a
//!   keyboard event narrowed to one key (`keydown.Enter`, `keydown.Escape`,
//!   `keydown.Space` — any `KeyboardEvent.key` value), a click with a held
//!   modifier (`click.shift`, `click.ctrl`, `click.meta`, `click.alt`), or one of the special
//!   triggers `visible`, `enter`;
//! - an optional `@scope` suffix listens beyond the element: `click@outside`
//!   (a click anywhere else — closing a menu), `keydown.Escape@document`
//!   (anywhere on the page, except while typing in a field — global shortcuts);
//! - `event` is an event on the owning machine, or `machine:event` /
//!   `machine#1:event` for another machine on the page.
//!
//! ```text
//! fx-on="keydown.Enter->save keydown.Escape->cancel"
//! fx-on="click.shift->select_range click->toggle"   (first match wins)
//! fx-on="keydown.Escape@document->close"
//! ```

/// One `trigger->event` binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxOn {
    /// `click`, `keydown`, `visible`, `click@outside`, …
    pub trigger: String,
    /// The key a keyboard trigger is limited to (`Enter` in `keydown.Enter`).
    pub key: Option<String>,
    /// Where it listens beyond the element: `outside`, `document`.
    pub scope: Option<String>,
    /// `save`, or `other:save` for another machine.
    pub event: String,
}

impl FxOn {
    /// Whether this binding handles a DOM event of type `kind`. `key` is the
    /// event's `KeyboardEvent.key` (`None` for non-keyboard events); a binding
    /// without a key filter matches any key.
    pub fn matches(&self, kind: &str, key: Option<&str>) -> bool {
        if self.trigger != kind {
            return false;
        }
        match (self.key.as_deref(), key) {
            (None, _) => true,
            // `Space` stands in for `" "`, which can't appear in the attribute.
            (Some("Space"), Some(k)) => k == " ",
            (Some(want), Some(k)) => want == k,
            (Some(_), None) => false,
        }
    }
}

/// Parse an `fx-on` attribute value. Malformed bindings (no `->`, empty
/// trigger or event) are skipped.
pub fn parse_fx_on(attr: &str) -> Vec<FxOn> {
    attr.split_whitespace()
        .filter_map(|spec| {
            let (trigger, event) = spec.split_once("->")?;
            let (trigger, scope) = match trigger.split_once('@') {
                Some((t, sc)) => (t, Some(sc.to_string()).filter(|sc| !sc.is_empty())),
                None => (trigger, None),
            };
            let (trigger, key) = match trigger.split_once('.') {
                Some((t, k)) => (t, Some(k.to_string()).filter(|k| !k.is_empty())),
                None => (trigger, None),
            };
            (!trigger.is_empty() && !event.is_empty())
                .then(|| FxOn { trigger: trigger.to_string(), key, scope, event: event.to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(trigger: &str, key: Option<&str>, event: &str) -> FxOn {
        FxOn { trigger: trigger.into(), key: key.map(Into::into), scope: None, event: event.into() }
    }

    #[test]
    fn single_binding() {
        assert_eq!(parse_fx_on("click->increment"), vec![on("click", None, "increment")]);
        assert_eq!(parse_fx_on("click@outside->close"), vec![FxOn { scope: Some("outside".into()), ..on("click", None, "close") }]);
        assert_eq!(
            parse_fx_on("keydown.Escape@document->close"),
            vec![FxOn { scope: Some("document".into()), ..on("keydown", Some("Escape"), "close") }]
        );
        assert_eq!(parse_fx_on("click->theme:toggle"), vec![on("click", None, "theme:toggle")]);
    }

    #[test]
    fn several_bindings_with_keys() {
        assert_eq!(
            parse_fx_on("keydown.Enter->save  keydown.Escape->cancel input->draft"),
            vec![on("keydown", Some("Enter"), "save"), on("keydown", Some("Escape"), "cancel"), on("input", None, "draft")]
        );
    }

    #[test]
    fn malformed_bindings_skipped() {
        assert_eq!(parse_fx_on("click save ->x click-> ->"), vec![]);
        assert_eq!(parse_fx_on("keydown.->x"), vec![on("keydown", None, "x")]);
    }

    #[test]
    fn matching() {
        let enter = on("keydown", Some("Enter"), "save");
        assert!(enter.matches("keydown", Some("Enter")));
        assert!(!enter.matches("keydown", Some("Escape")));
        assert!(!enter.matches("keyup", Some("Enter")));
        assert!(!enter.matches("keydown", None));
        assert!(on("keydown", None, "x").matches("keydown", Some("a")));
        assert!(on("click", None, "x").matches("click", None));
        assert!(on("keydown", Some("Space"), "x").matches("keydown", Some(" ")));
    }
}
