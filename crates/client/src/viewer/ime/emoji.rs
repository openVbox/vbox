//! macOS Character Viewer (emoji picker) output handling.
//!
//! `Ctrl+Cmd+Space` opens macOS' Character Viewer; selecting a glyph delivers
//! a fully composed string through `Ime::Commit`. The bytes already represent
//! the intended emoji — there is no jamo-style assembly to do — but the
//! commit can carry a multi-codepoint sequence (ZWJ joins, regional-indicator
//! pairs, skin-tone modifiers, variation selectors) that must travel to the
//! guest as a single grapheme. This module is the one place that:
//!
//! - identifies whether a chunk is emoji-bearing so the host can route it
//!   through a non-composing path and tag debug logs accordingly,
//! - splits text into grapheme-cluster–like chunks that keep ZWJ and
//!   variation-selector tails attached to their base codepoint, so we never
//!   ship a half 🇰🇷 or a head-without-modifier 🤚,
//! - dumps codepoints in a stable form for the debug bundle.
//!
//! The detector is a deliberate over-approximation: any code point likely to
//! belong to a picker emission is treated as emoji-ish. False positives at the
//! edges (a stray dingbat) are harmless — the routing is identical.

/// True when `ch` plausibly belongs to a picker emoji emission.
///
/// Covers the supplementary-plane emoji blocks plus the BMP dingbats /
/// miscellaneous-symbols ranges Apple's picker draws from, and the join /
/// modifier code points (ZWJ, variation selectors, skin-tone modifiers,
/// regional indicators) that glue multi-codepoint glyphs together.
pub fn is_emoji(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        // Supplementary-plane emoji blocks (smileys, transport, food, flags
        // via regional indicators, supplemental symbols/pictographs, …).
        0x1F000..=0x1FFFF
        // BMP dingbats and miscellaneous symbols Apple's picker uses
        // (☂, ⚠, ✈, ❤, ★, …).
        | 0x2300..=0x23FF
        | 0x2500..=0x27BF
        | 0x2900..=0x29FF
        | 0x2B00..=0x2BFF
        // Glue + presentation selectors that appear inside picker output.
        | 0x200D
        | 0xFE0E..=0xFE0F
    )
}

/// True when the chunk contains at least one emoji-ish code point.
pub fn contains_emoji(text: &str) -> bool {
    text.chars().any(is_emoji)
}

/// Split `text` into chunks where each chunk is one emoji grapheme (base
/// code point with any trailing variation selectors, skin-tone modifiers, or
/// ZWJ-joined continuations) or a single non-emoji code point.
///
/// This avoids the workspace pulling in a full grapheme-segmentation
/// dependency: the rule we need is narrow — keep the emoji "tail" stuck to
/// its base — and it is local enough to express directly.
pub fn split_clusters(text: &str) -> Vec<String> {
    let mut clusters: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut expecting_continuation = false;

    for ch in text.chars() {
        if is_continuation(ch) {
            // A continuation glyph attaches to the current cluster if there is
            // one; otherwise it stands alone (defensive — Apple's picker
            // never emits a leading combiner, but a corrupt frame could).
            if current.is_empty() {
                clusters.push(ch.to_string());
            } else {
                current.push(ch);
                expecting_continuation = is_zwj(ch);
            }
            continue;
        }

        if expecting_continuation {
            // The previous code point was a ZWJ — this one closes the join,
            // regardless of whether it is emoji or not.
            current.push(ch);
            expecting_continuation = false;
            continue;
        }

        if let Some(prev) = current.chars().last() {
            if is_regional_indicator(prev)
                && is_regional_indicator(ch)
                && cluster_flag_open(&current)
            {
                // Pair the second half of a regional-indicator flag onto the
                // first half so 🇰🇷 stays one cluster.
                current.push(ch);
                continue;
            }
        }

        if !current.is_empty() {
            clusters.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        clusters.push(current);
    }
    clusters
}

/// Stable lowercase-hex dump of every code point in `text`, e.g.
/// `"U+1F44B U+1F3FB"`. Used by the debug logger so picker output that goes
/// wrong on the wire can be recognised in `client.log`.
pub fn codepoint_dump(text: &str) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i != 0 {
            out.push(' ');
        }
        out.push_str(&format!("U+{:04X}", ch as u32));
    }
    out
}

fn is_zwj(ch: char) -> bool {
    ch as u32 == 0x200D
}

fn is_variation_selector(ch: char) -> bool {
    matches!(ch as u32, 0xFE0E..=0xFE0F)
}

fn is_skin_tone(ch: char) -> bool {
    matches!(ch as u32, 0x1F3FB..=0x1F3FF)
}

fn is_regional_indicator(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

fn is_continuation(ch: char) -> bool {
    is_zwj(ch) || is_variation_selector(ch) || is_skin_tone(ch)
}

/// True when `cluster` is a single regional indicator (i.e. waiting for its
/// pair to land). Two indicators form a flag; a third would start a new flag.
fn cluster_flag_open(cluster: &str) -> bool {
    let mut iter = cluster.chars();
    let Some(first) = iter.next() else {
        return false;
    };
    is_regional_indicator(first) && iter.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_basic_smiley() {
        assert!(is_emoji('😀'));
        assert!(contains_emoji("hi 😀"));
    }

    #[test]
    fn ignores_plain_ascii_and_hangul() {
        assert!(!is_emoji('a'));
        assert!(!is_emoji('가'));
        assert!(!contains_emoji("hello"));
        assert!(!contains_emoji("한글"));
        assert!(!contains_emoji(""));
    }

    #[test]
    fn keeps_zwj_family_as_one_cluster() {
        // 👨‍👩‍👧 — three people glued by ZWJs.
        let text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(split_clusters(text), vec![text.to_string()]);
    }

    #[test]
    fn keeps_skin_tone_attached() {
        // 🤚🏿 — raised hand with dark skin tone modifier.
        let text = "\u{1F91A}\u{1F3FF}";
        assert_eq!(split_clusters(text), vec![text.to_string()]);
    }

    #[test]
    fn keeps_variation_selector_attached() {
        // ❤️ — heart + emoji presentation selector.
        let text = "\u{2764}\u{FE0F}";
        assert_eq!(split_clusters(text), vec![text.to_string()]);
    }

    #[test]
    fn pairs_regional_indicator_flag() {
        // 🇰🇷 — Korea: U+1F1F0 + U+1F1F7. Two flags in a row must split.
        let kr = "\u{1F1F0}\u{1F1F7}";
        let jp = "\u{1F1EF}\u{1F1F5}";
        let combined = format!("{kr}{jp}");
        assert_eq!(
            split_clusters(&combined),
            vec![kr.to_string(), jp.to_string()]
        );
    }

    #[test]
    fn splits_emoji_from_surrounding_text() {
        let text = "hi 😀!";
        let clusters = split_clusters(text);
        assert_eq!(clusters, vec!["h", "i", " ", "😀", "!"]);
    }

    #[test]
    fn codepoint_dump_is_stable() {
        // 👋🏻 — wave + light skin tone.
        assert_eq!(codepoint_dump("\u{1F44B}\u{1F3FB}"), "U+1F44B U+1F3FB");
        assert_eq!(codepoint_dump(""), "");
    }

    #[test]
    fn defensive_leading_continuation_is_isolated() {
        // A stray ZWJ at the start should not panic and should stand alone.
        let text = "\u{200D}😀";
        assert_eq!(
            split_clusters(text),
            vec!["\u{200D}".to_string(), "😀".to_string()]
        );
    }
}
