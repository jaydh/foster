pub mod machine;
pub mod snapshot;

pub use foster_macros::html;
pub use machine::{Machine, MachineBuilder, MachineError, MachineInstance, TransitionDef};
pub use snapshot::Snapshot;

/// Wrap body HTML in a standard page shell (DOCTYPE, head, CSS, script).
///
/// The `style` parameter is embedded in a `<style>` block.  Pass an empty string
/// or `include_str!("../static/style.css")` as appropriate.
pub fn page(title: &str, style: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<style>[fx-show]{{display:none}}{style}</style>
</head>
<body>
{body}
<script type="module">import init from '/pkg/foster_client.js';init();</script>
</body>
</html>"#
    )
}
