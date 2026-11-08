//! Launch metadata recorded in a session's first prompt.
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// How far into the full first user message a stamp is looked for: T3's role
/// preamble and Telepathica's delivery header (about 600 characters) come first.
pub const WINDOW: usize = 4000;

/// The four row fields from the full first message, as adapters set them.
pub fn fields(text: &str) -> [(&'static str, Value); 4] {
    let head: String = text.chars().take(WINDOW).collect();
    let launch = parse(&head).unwrap_or_default();
    [
        ("launch_surface", "surface"),
        ("launch_subject", "subject"),
        ("launch_launcher", "launcher"),
        ("launch_parent", "parent"),
    ]
    .map(|(field, key)| (field, launch.get(key).cloned().unwrap_or(Value::Null)))
}

pub fn parse(text: &str) -> Option<Map<String, Value>> {
    static STAMP: OnceLock<regex::Regex> = OnceLock::new();
    let captures = STAMP
        .get_or_init(|| {
            regex::Regex::new(r"(?m)^\[harbor-launch v1((?: [a-z_]+=[A-Za-z0-9\-_.:/#@+%]+)+)\]")
                .unwrap()
        })
        .captures(text)?;
    Some(
        captures[1]
            .split_whitespace()
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap();
                let decoded = percent_encoding::percent_decode_str(value)
                    .decode_utf8()
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| value.to_owned());
                (key.to_owned(), Value::String(decoded))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stamps() {
        assert_eq!(parse("[harbor-launch v1 surface=bosun subject=case:01ABC launcher=tick parent=claude:abc@host]").unwrap(),
            json!({"surface":"bosun","subject":"case:01ABC","launcher":"tick","parent":"claude:abc@host"}).as_object().unwrap().clone());
        assert_eq!(parse("[telepathica] Message m_1 from claude:x@h (handoff)\n[harbor-launch v1 surface=lore subject=none launcher=handoff]").unwrap()["surface"], "lore");
        assert_eq!(
            parse("[harbor-launch v1 subject=goal:fix%20it]").unwrap()["subject"],
            "goal:fix it"
        );
        assert_eq!(
            parse("[harbor-launch v1 surface=first]\n[harbor-launch v1 surface=second]").unwrap()["surface"],
            "first"
        );
        // A stamp quoted mid-line (an example in prose) is not a launch stamp.
        assert!(parse("see `[harbor-launch v1 surface=quoted]` for the format").is_none());
        assert_eq!(
            parse("see [harbor-launch v1 surface=quoted]\n[harbor-launch v1 surface=real]")
                .unwrap()["surface"],
            "real"
        );
        assert_eq!(
            parse("[harbor-launch v1 extra=%E2%9C%93]").unwrap()["extra"],
            "✓"
        );
        assert_eq!(
            parse("[harbor-launch v1 extra=%FF]").unwrap()["extra"],
            "%FF"
        );
        assert_eq!(
            parse("[harbor-launch v1 extra=%GG]").unwrap()["extra"],
            "%GG"
        );
        for text in [
            "[harbor-launch v2 surface=x]",
            "[harbor-launch v1]",
            "[harbor-launch v1 surface=x",
        ] {
            assert!(parse(text).is_none());
        }
    }
}
