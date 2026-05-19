//! Japanese kana voicing composer.
//!
//! Some IMEs deliver kana with a separate voicing or semi-voicing mark
//! (`か` + `\u{3099}` instead of `が`). This composer pairs a buffered kana
//! base with the trailing mark so the guest only ever sees pre-composed
//! glyphs. Combining marks (U+3099/U+309A) and their spacing siblings
//! (U+309B/U+309C) are both accepted.

use super::ComposerAction;

#[derive(Debug, Default)]
pub struct KanaComposer {
    pending: Option<char>,
}

impl KanaComposer {
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
    }

    /// True when the chunk is entirely kana-like *and* contains at least one
    /// voicing mark — i.e. the composer has work to do. Pre-composed kana is
    /// left alone and forwarded directly by the dispatcher.
    pub fn can_compose(text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let mut has_mark = false;
        for ch in text.chars() {
            if !is_kana_or_mark(ch) {
                return false;
            }
            if is_voicing_mark(ch) {
                has_mark = true;
            }
        }
        has_mark
    }

    pub fn push_text(&mut self, text: &str) -> Vec<ComposerAction> {
        let mut actions = Vec::new();
        for ch in text.chars() {
            self.push_char(ch, &mut actions);
        }
        actions.push(ComposerAction::Preedit(self.preedit_text()));
        actions
    }

    pub fn flush(&mut self) -> Vec<ComposerAction> {
        let mut actions = Vec::new();
        if let Some(ch) = self.pending.take() {
            actions.push(ComposerAction::Commit(ch.to_string()));
            actions.push(ComposerAction::Preedit(None));
        }
        actions
    }

    pub fn backspace(&mut self) -> Vec<ComposerAction> {
        if self.pending.take().is_some() {
            vec![ComposerAction::Preedit(None)]
        } else {
            Vec::new()
        }
    }

    fn push_char(&mut self, ch: char, actions: &mut Vec<ComposerAction>) {
        if is_voicing_mark(ch) {
            match self.pending.take() {
                Some(base) => match apply_mark(base, ch) {
                    Some(out) => actions.push(ComposerAction::Commit(out.to_string())),
                    None => {
                        actions.push(ComposerAction::Commit(base.to_string()));
                        actions.push(ComposerAction::Commit(ch.to_string()));
                    }
                },
                None => actions.push(ComposerAction::Commit(ch.to_string())),
            }
            return;
        }

        if let Some(base) = self.pending.take() {
            actions.push(ComposerAction::Commit(base.to_string()));
        }
        if is_kana_base(ch) {
            self.pending = Some(ch);
        } else {
            actions.push(ComposerAction::Commit(ch.to_string()));
        }
    }

    fn preedit_text(&self) -> Option<String> {
        self.pending.map(|c| c.to_string())
    }
}

fn is_kana_base(ch: char) -> bool {
    let code = ch as u32;
    let in_hiragana = (0x3040..=0x309F).contains(&code);
    let in_katakana = (0x30A0..=0x30FF).contains(&code);
    let in_phonetic_ext = (0x31F0..=0x31FF).contains(&code);
    (in_hiragana || in_katakana || in_phonetic_ext) && !is_voicing_mark(ch)
}

fn is_voicing_mark(ch: char) -> bool {
    matches!(ch, '\u{3099}' | '\u{309A}' | '\u{309B}' | '\u{309C}')
}

fn is_kana_or_mark(ch: char) -> bool {
    is_kana_base(ch) || is_voicing_mark(ch)
}

fn apply_mark(base: char, mark: char) -> Option<char> {
    let want_semi = matches!(mark, '\u{309A}' | '\u{309C}');
    let entry = VOICING_TABLE.iter().find(|(b, _, _)| *b == base)?;
    if want_semi { entry.2 } else { entry.1 }
}

// (base, voiced, semi-voiced)
const VOICING_TABLE: &[(char, Option<char>, Option<char>)] = &[
    // Hiragana
    ('う', Some('ゔ'), None),
    ('か', Some('が'), None),
    ('き', Some('ぎ'), None),
    ('く', Some('ぐ'), None),
    ('け', Some('げ'), None),
    ('こ', Some('ご'), None),
    ('さ', Some('ざ'), None),
    ('し', Some('じ'), None),
    ('す', Some('ず'), None),
    ('せ', Some('ぜ'), None),
    ('そ', Some('ぞ'), None),
    ('た', Some('だ'), None),
    ('ち', Some('ぢ'), None),
    ('つ', Some('づ'), None),
    ('て', Some('で'), None),
    ('と', Some('ど'), None),
    ('は', Some('ば'), Some('ぱ')),
    ('ひ', Some('び'), Some('ぴ')),
    ('ふ', Some('ぶ'), Some('ぷ')),
    ('へ', Some('べ'), Some('ぺ')),
    ('ほ', Some('ぼ'), Some('ぽ')),
    // Katakana
    ('ウ', Some('ヴ'), None),
    ('カ', Some('ガ'), None),
    ('キ', Some('ギ'), None),
    ('ク', Some('グ'), None),
    ('ケ', Some('ゲ'), None),
    ('コ', Some('ゴ'), None),
    ('サ', Some('ザ'), None),
    ('シ', Some('ジ'), None),
    ('ス', Some('ズ'), None),
    ('セ', Some('ゼ'), None),
    ('ソ', Some('ゾ'), None),
    ('タ', Some('ダ'), None),
    ('チ', Some('ヂ'), None),
    ('ツ', Some('ヅ'), None),
    ('テ', Some('デ'), None),
    ('ト', Some('ド'), None),
    ('ハ', Some('バ'), Some('パ')),
    ('ヒ', Some('ビ'), Some('ピ')),
    ('フ', Some('ブ'), Some('プ')),
    ('ヘ', Some('ベ'), Some('ペ')),
    ('ホ', Some('ボ'), Some('ポ')),
    ('ワ', Some('ヷ'), None),
    ('ヰ', Some('ヸ'), None),
    ('ヱ', Some('ヹ'), None),
    ('ヲ', Some('ヺ'), None),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(input: &str) -> String {
        let mut composer = KanaComposer::default();
        composer
            .push_text(input)
            .into_iter()
            .chain(composer.flush())
            .filter_map(|a| match a {
                ComposerAction::Commit(text) => Some(text),
                ComposerAction::Preedit(_) => None,
            })
            .collect()
    }

    #[test]
    fn composes_voiced_hiragana() {
        assert_eq!(committed("か\u{3099}"), "が");
        assert_eq!(committed("し\u{3099}そ\u{3099}"), "じぞ");
    }

    #[test]
    fn composes_semi_voiced_hiragana() {
        assert_eq!(committed("は\u{309A}"), "ぱ");
        assert_eq!(committed("ひ\u{309A}"), "ぴ");
    }

    #[test]
    fn composes_spacing_voicing_mark() {
        assert_eq!(committed("か\u{309B}"), "が");
        assert_eq!(committed("は\u{309C}"), "ぱ");
    }

    #[test]
    fn composes_katakana() {
        assert_eq!(committed("カ\u{3099}"), "ガ");
        assert_eq!(committed("ハ\u{309A}"), "パ");
    }

    #[test]
    fn passes_through_unpairable_marks() {
        // semi-voicing has no effect on か — emit both glyphs unchanged.
        assert_eq!(committed("か\u{309A}"), "か\u{309A}");
    }

    #[test]
    fn detection_requires_a_mark() {
        assert!(!KanaComposer::can_compose(""));
        assert!(!KanaComposer::can_compose("か"));
        assert!(KanaComposer::can_compose("か\u{3099}"));
        assert!(!KanaComposer::can_compose("hello"));
        assert!(!KanaComposer::can_compose("ㄱㅏ"));
    }

    #[test]
    fn is_empty_tracks_pending_base() {
        let mut composer = KanaComposer::default();
        assert!(composer.is_empty());
        let _ = composer.push_text("か\u{3099}");
        // After a successful pair the pending base is consumed.
        assert!(composer.is_empty());
    }

    #[test]
    fn backspace_on_empty_state_is_a_noop() {
        let mut composer = KanaComposer::default();
        assert!(composer.backspace().is_empty());
    }

    #[test]
    fn backspace_drops_pending_base() {
        // Feed a bare base char so the composer holds it in preedit; voicing
        // mark never arrives, then backspace clears the pending state.
        let mut composer = KanaComposer::default();
        let _ = composer.push_text("か");
        assert!(!composer.is_empty());
        assert_eq!(composer.backspace(), vec![ComposerAction::Preedit(None)]);
        assert!(composer.is_empty());
    }
}
