//! Profile-field extraction from `Found` pages.
//!
//! Given a site's [`Extractor`] rules and a response body, pull out
//! structured fields (name, bio, avatar URL, …) using CSS selectors. This
//! runs only under `--enrich` and only for `Found` outcomes, so the cost is
//! bounded to the handful of sites where an account exists.
//!
//! Robustness:
//! - The body is truncated to [`MAX_PARSE_BYTES`] before parsing, capping
//!   parser time/memory on hostile or accidentally huge pages.
//! - Extracted values are trimmed, whitespace-collapsed, and length-capped.
//! - A selector that matches nothing simply yields no field (graceful).

use std::collections::BTreeMap;

use scraper::{Html, Selector};

use crate::site::Extractor;

/// Upper bound on the body we feed to the HTML parser.
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound on a single extracted value.
const MAX_VALUE_LEN: usize = 512;

/// Run `extractors` against `body`, returning the fields that matched.
///
/// Selectors are assumed valid (the registry validates them at load via
/// [`crate::Site::validate`]); an invalid one here is skipped defensively.
pub(crate) fn extract(body: &str, extractors: &[Extractor]) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if extractors.is_empty() {
        return fields;
    }
    let truncated = truncate_on_char_boundary(body, MAX_PARSE_BYTES);
    let document = Html::parse_document(truncated);

    for extractor in extractors {
        let Ok(selector) = Selector::parse(&extractor.selector) else {
            continue;
        };
        let Some(element) = document.select(&selector).next() else {
            continue;
        };
        let raw = extractor.attr.as_deref().map_or_else(
            || Some(element.text().collect::<String>()),
            |attr| element.value().attr(attr).map(str::to_owned),
        );
        if let Some(value) = raw {
            let cleaned = clean(&value);
            if !cleaned.is_empty() {
                fields.insert(extractor.field.clone(), cleaned);
            }
        }
    }
    fields
}

/// Collapse runs of whitespace, trim, and cap length.
fn clean(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_VALUE_LEN).collect()
}

fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extractor(field: &str, selector: &str, attr: Option<&str>) -> Extractor {
        Extractor {
            field: field.into(),
            selector: selector.into(),
            attr: attr.map(str::to_owned),
        }
    }

    const PROFILE: &str = r#"
        <html><head><title>alice</title></head><body>
          <h1 class="name">Alice Liddell</h1>
          <p class="bio">  Curiouser   and
          curiouser.  </p>
          <img class="avatar" src="https://cdn.example.com/a.png" alt="x">
        </body></html>
    "#;

    #[test]
    fn extracts_text_and_attribute_fields() {
        let rules = vec![
            extractor("name", "h1.name", None),
            extractor("bio", "p.bio", None),
            extractor("avatar", "img.avatar", Some("src")),
        ];
        let fields = extract(PROFILE, &rules);
        assert_eq!(fields.get("name").unwrap(), "Alice Liddell");
        // whitespace collapsed across the newline
        assert_eq!(fields.get("bio").unwrap(), "Curiouser and curiouser.");
        assert_eq!(
            fields.get("avatar").unwrap(),
            "https://cdn.example.com/a.png"
        );
    }

    #[test]
    fn missing_selector_yields_no_field() {
        let rules = vec![extractor("nope", ".does-not-exist", None)];
        assert!(extract(PROFILE, &rules).is_empty());
    }

    #[test]
    fn missing_attribute_yields_no_field() {
        let rules = vec![extractor("title", "img.avatar", Some("data-nonexistent"))];
        assert!(extract(PROFILE, &rules).is_empty());
    }

    #[test]
    fn invalid_selector_is_skipped() {
        let rules = vec![
            extractor("bad", ">>>not a selector", None),
            extractor("name", "h1.name", None),
        ];
        let fields = extract(PROFILE, &rules);
        assert!(!fields.contains_key("bad"));
        assert_eq!(fields.get("name").unwrap(), "Alice Liddell");
    }

    #[test]
    fn empty_extractors_returns_empty() {
        assert!(extract(PROFILE, &[]).is_empty());
    }

    #[test]
    fn long_value_is_capped() {
        let body = format!("<p class=\"bio\">{}</p>", "x".repeat(2000));
        let rules = vec![extractor("bio", "p.bio", None)];
        let fields = extract(&body, &rules);
        assert_eq!(fields.get("bio").unwrap().chars().count(), MAX_VALUE_LEN);
    }

    #[test]
    fn truncation_respects_char_boundary() {
        // Multibyte chars near the cap must not panic.
        let s = "é".repeat(10);
        let t = truncate_on_char_boundary(&s, 5);
        assert!(s.starts_with(t));
        assert!(t.len() <= 5);
    }

    #[test]
    fn truncation_handles_mixed_charset_around_boundary() {
        // Real-world bios mix ASCII with multi-byte codepoints — emoji
        // (4-byte), CJK (3-byte), Cyrillic / Latin-with-diacritic
        // (2-byte). Truncation must never split a codepoint, even when
        // the requested cut lands at a boundary inside the next char.
        let s = "abc🎉де中f"; // 3 ASCII + 4-byte emoji + 2×2-byte Cyrillic + 3-byte CJK + 1 ASCII
        for cut in 0..=s.len() {
            let t = truncate_on_char_boundary(s, cut);
            assert!(s.is_char_boundary(t.len()), "cut {cut} not on boundary");
            assert!(t.len() <= cut, "cut {cut} produced {} bytes", t.len());
            assert!(s.starts_with(t), "cut {cut} returned non-prefix");
            // The returned slice must round-trip as valid UTF-8.
            assert_eq!(std::str::from_utf8(t.as_bytes()).unwrap(), t);
        }
    }
}
