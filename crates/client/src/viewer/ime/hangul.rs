//! Hangul compatibility-jamo composer.
//!
//! macOS' 2-set Korean IME emits compatibility jamo (U+3130..=U+318F) and
//! expects the application to assemble syllables (U+AC00..=U+D7A3). Each
//! incoming chunk is fed through this state machine; partial syllables stay
//! in the preedit until they are finalised by another chunk or by `flush()`.

use super::ComposerAction;

#[derive(Debug, Default)]
pub struct HangulComposer {
    pending: Pending,
}

#[derive(Debug, Default, Clone, Copy)]
enum Pending {
    #[default]
    None,
    Initial {
        l: usize,
    },
    Vowel {
        v: usize,
    },
    Syllable {
        l: usize,
        v: usize,
        t: usize,
    },
}

impl HangulComposer {
    pub fn is_empty(&self) -> bool {
        matches!(self.pending, Pending::None)
    }

    pub fn can_compose(text: &str) -> bool {
        !text.is_empty() && text.chars().all(is_compat_jamo)
    }

    pub fn push_text(&mut self, text: &str) -> Vec<ComposerAction> {
        let mut actions = Vec::new();
        for ch in text.chars() {
            actions.extend(self.push_char(ch));
        }
        actions
    }

    pub fn flush(&mut self) -> Vec<ComposerAction> {
        let mut actions = Vec::new();
        if let Some(text) = self.take_pending_text() {
            actions.push(ComposerAction::Commit(text));
            actions.push(ComposerAction::Preedit(None));
        }
        actions
    }

    pub fn backspace(&mut self) -> Vec<ComposerAction> {
        match self.pending {
            Pending::None => Vec::new(),
            Pending::Initial { .. } | Pending::Vowel { .. } => {
                self.pending = Pending::None;
                vec![ComposerAction::Preedit(None)]
            }
            Pending::Syllable { l, v, t } if t != 0 => {
                self.pending = Pending::Syllable { l, v, t: 0 };
                vec![ComposerAction::Preedit(self.preedit_text())]
            }
            Pending::Syllable { l, .. } => {
                self.pending = Pending::Initial { l };
                vec![ComposerAction::Preedit(self.preedit_text())]
            }
        }
    }

    fn push_char(&mut self, ch: char) -> Vec<ComposerAction> {
        let mut actions = Vec::new();
        if let Some(l) = initial_index(ch) {
            self.push_consonant(ch, l, &mut actions);
        } else if let Some(v) = vowel_index(ch) {
            self.push_vowel(v, &mut actions);
        } else {
            if let Some(text) = self.take_pending_text() {
                actions.push(ComposerAction::Commit(text));
            }
            actions.push(ComposerAction::Commit(ch.to_string()));
        }
        actions.push(ComposerAction::Preedit(self.preedit_text()));
        actions
    }

    fn push_consonant(&mut self, ch: char, l_new: usize, actions: &mut Vec<ComposerAction>) {
        match self.pending {
            Pending::None => self.pending = Pending::Initial { l: l_new },
            Pending::Initial { .. } | Pending::Vowel { .. } => {
                if let Some(text) = self.take_pending_text() {
                    actions.push(ComposerAction::Commit(text));
                }
                self.pending = Pending::Initial { l: l_new };
            }
            Pending::Syllable { l, v, t: 0 } => {
                if let Some(t_new) = final_index(ch) {
                    self.pending = Pending::Syllable { l, v, t: t_new };
                } else {
                    if let Some(text) = self.take_pending_text() {
                        actions.push(ComposerAction::Commit(text));
                    }
                    self.pending = Pending::Initial { l: l_new };
                }
            }
            Pending::Syllable { l, v, t } => {
                if let Some(t_new) = final_index(ch).and_then(|next| combine_final(t, next)) {
                    self.pending = Pending::Syllable { l, v, t: t_new };
                } else {
                    if let Some(text) = self.take_pending_text() {
                        actions.push(ComposerAction::Commit(text));
                    }
                    self.pending = Pending::Initial { l: l_new };
                }
            }
        }
    }

    fn push_vowel(&mut self, v_new: usize, actions: &mut Vec<ComposerAction>) {
        match self.pending {
            Pending::None => self.pending = Pending::Vowel { v: v_new },
            Pending::Initial { l } => self.pending = Pending::Syllable { l, v: v_new, t: 0 },
            Pending::Vowel { v } => {
                if let Some(v) = combine_vowel(v, v_new) {
                    self.pending = Pending::Vowel { v };
                } else {
                    if let Some(text) = self.take_pending_text() {
                        actions.push(ComposerAction::Commit(text));
                    }
                    self.pending = Pending::Vowel { v: v_new };
                }
            }
            Pending::Syllable { l, v, t: 0 } => {
                if let Some(v) = combine_vowel(v, v_new) {
                    self.pending = Pending::Syllable { l, v, t: 0 };
                } else {
                    if let Some(text) = self.take_pending_text() {
                        actions.push(ComposerAction::Commit(text));
                    }
                    self.pending = Pending::Vowel { v: v_new };
                }
            }
            Pending::Syllable { l, v, t } => {
                let (remaining_t, moved_l) = split_final(t);
                actions.push(ComposerAction::Commit(compose_syllable(l, v, remaining_t)));
                self.pending = Pending::Syllable {
                    l: moved_l,
                    v: v_new,
                    t: 0,
                };
            }
        }
    }

    fn take_pending_text(&mut self) -> Option<String> {
        let text = self.pending_text();
        self.pending = Pending::None;
        text
    }

    fn preedit_text(&self) -> Option<String> {
        self.pending_text()
    }

    fn pending_text(&self) -> Option<String> {
        match self.pending {
            Pending::None => None,
            Pending::Initial { l } => Some(initial_char(l).to_string()),
            Pending::Vowel { v } => Some(vowel_char(v).to_string()),
            Pending::Syllable { l, v, t } => Some(compose_syllable(l, v, t)),
        }
    }
}

fn is_compat_jamo(ch: char) -> bool {
    matches!(ch as u32, 0x3130..=0x318F)
}

fn initial_index(ch: char) -> Option<usize> {
    Some(match ch {
        'ㄱ' => 0,
        'ㄲ' => 1,
        'ㄴ' => 2,
        'ㄷ' => 3,
        'ㄸ' => 4,
        'ㄹ' => 5,
        'ㅁ' => 6,
        'ㅂ' => 7,
        'ㅃ' => 8,
        'ㅅ' => 9,
        'ㅆ' => 10,
        'ㅇ' => 11,
        'ㅈ' => 12,
        'ㅉ' => 13,
        'ㅊ' => 14,
        'ㅋ' => 15,
        'ㅌ' => 16,
        'ㅍ' => 17,
        'ㅎ' => 18,
        _ => return None,
    })
}

fn initial_char(l: usize) -> char {
    [
        'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ',
        'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
    ][l]
}

fn vowel_index(ch: char) -> Option<usize> {
    Some(match ch {
        'ㅏ' => 0,
        'ㅐ' => 1,
        'ㅑ' => 2,
        'ㅒ' => 3,
        'ㅓ' => 4,
        'ㅔ' => 5,
        'ㅕ' => 6,
        'ㅖ' => 7,
        'ㅗ' => 8,
        'ㅘ' => 9,
        'ㅙ' => 10,
        'ㅚ' => 11,
        'ㅛ' => 12,
        'ㅜ' => 13,
        'ㅝ' => 14,
        'ㅞ' => 15,
        'ㅟ' => 16,
        'ㅠ' => 17,
        'ㅡ' => 18,
        'ㅢ' => 19,
        'ㅣ' => 20,
        _ => return None,
    })
}

fn vowel_char(v: usize) -> char {
    [
        'ㅏ', 'ㅐ', 'ㅑ', 'ㅒ', 'ㅓ', 'ㅔ', 'ㅕ', 'ㅖ', 'ㅗ', 'ㅘ', 'ㅙ', 'ㅚ', 'ㅛ', 'ㅜ', 'ㅝ',
        'ㅞ', 'ㅟ', 'ㅠ', 'ㅡ', 'ㅢ', 'ㅣ',
    ][v]
}

fn final_index(ch: char) -> Option<usize> {
    Some(match ch {
        'ㄱ' => 1,
        'ㄲ' => 2,
        'ㄳ' => 3,
        'ㄴ' => 4,
        'ㄵ' => 5,
        'ㄶ' => 6,
        'ㄷ' => 7,
        'ㄹ' => 8,
        'ㄺ' => 9,
        'ㄻ' => 10,
        'ㄼ' => 11,
        'ㄽ' => 12,
        'ㄾ' => 13,
        'ㄿ' => 14,
        'ㅀ' => 15,
        'ㅁ' => 16,
        'ㅂ' => 17,
        'ㅄ' => 18,
        'ㅅ' => 19,
        'ㅆ' => 20,
        'ㅇ' => 21,
        'ㅈ' => 22,
        'ㅊ' => 23,
        'ㅋ' => 24,
        'ㅌ' => 25,
        'ㅍ' => 26,
        'ㅎ' => 27,
        _ => return None,
    })
}

fn compose_syllable(l: usize, v: usize, t: usize) -> String {
    let code = 0xAC00 + ((l as u32 * 21 + v as u32) * 28 + t as u32);
    char::from_u32(code).unwrap_or('\u{FFFD}').to_string()
}

fn combine_vowel(left: usize, right: usize) -> Option<usize> {
    Some(match (left, right) {
        (8, 0) => 9,
        (8, 1) => 10,
        (8, 20) => 11,
        (13, 4) => 14,
        (13, 5) => 15,
        (13, 20) => 16,
        (18, 20) => 19,
        _ => return None,
    })
}

fn combine_final(left: usize, right: usize) -> Option<usize> {
    Some(match (left, right) {
        (1, 19) => 3,
        (4, 22) => 5,
        (4, 27) => 6,
        (8, 1) => 9,
        (8, 16) => 10,
        (8, 17) => 11,
        (8, 19) => 12,
        (8, 25) => 13,
        (8, 26) => 14,
        (8, 27) => 15,
        (17, 19) => 18,
        _ => return None,
    })
}

fn split_final(t: usize) -> (usize, usize) {
    match t {
        3 => (1, 9),
        5 => (4, 12),
        6 => (4, 18),
        9 => (8, 0),
        10 => (8, 6),
        11 => (8, 7),
        12 => (8, 9),
        13 => (8, 16),
        14 => (8, 17),
        15 => (8, 18),
        18 => (17, 9),
        _ => (0, final_to_initial(t)),
    }
}

fn final_to_initial(t: usize) -> usize {
    match t {
        1 => 0,
        2 => 1,
        4 => 2,
        7 => 3,
        8 => 5,
        16 => 6,
        17 => 7,
        19 => 9,
        20 => 10,
        21 => 11,
        22 => 12,
        23 => 14,
        24 => 15,
        25 => 16,
        26 => 17,
        27 => 18,
        _ => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(input: &str) -> String {
        let mut composer = HangulComposer::default();
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
    fn composes_common_syllables() {
        assert_eq!(committed("ㅇㅏㄴㄴㅕㅇ"), "안녕");
        assert_eq!(committed("ㅎㅏㄴㄱㅡㄹ"), "한글");
    }

    #[test]
    fn splits_final_before_vowel() {
        assert_eq!(committed("ㄱㅏㄱㅣ"), "가기");
        assert_eq!(committed("ㅇㅏㄴㅈㅏ"), "안자");
    }

    #[test]
    fn combines_vowels_and_keeps_standalone_consonants() {
        assert_eq!(committed("ㄱㅗㅏ"), "과");
        assert_eq!(committed("ㅁㄴㅇㄹ"), "ㅁㄴㅇㄹ");
    }

    #[test]
    fn detects_compatibility_jamo_text() {
        assert!(HangulComposer::can_compose("ㄱㅏ"));
        assert!(!HangulComposer::can_compose("가"));
        assert!(!HangulComposer::can_compose("a"));
    }

    fn preedit_after(input: &str) -> Option<String> {
        let mut composer = HangulComposer::default();
        let _ = composer.push_text(input);
        match composer.backspace().last() {
            Some(ComposerAction::Preedit(text)) => text.clone(),
            _ => None,
        }
    }

    #[test]
    fn backspace_on_empty_state_is_a_noop() {
        let mut composer = HangulComposer::default();
        assert!(composer.backspace().is_empty());
    }

    #[test]
    fn backspace_clears_lone_initial_or_vowel() {
        // Lone initial: ㄱ → empty preedit
        let mut composer = HangulComposer::default();
        composer.push_text("ㄱ");
        assert_eq!(composer.backspace(), vec![ComposerAction::Preedit(None)]);
        assert!(composer.is_empty());

        // Lone vowel: ㅏ → empty preedit
        let mut composer = HangulComposer::default();
        composer.push_text("ㅏ");
        assert_eq!(composer.backspace(), vec![ComposerAction::Preedit(None)]);
        assert!(composer.is_empty());
    }

    #[test]
    fn backspace_drops_final_from_syllable() {
        // 간 (ㄱ+ㅏ+ㄴ) → backspace removes ㄴ, leaves 가.
        assert_eq!(preedit_after("ㄱㅏㄴ").as_deref(), Some("가"));
    }

    #[test]
    fn backspace_drops_vowel_from_syllable() {
        // 가 (ㄱ+ㅏ) → backspace removes ㅏ, leaves bare ㄱ.
        assert_eq!(preedit_after("ㄱㅏ").as_deref(), Some("ㄱ"));
    }

    // ---- Additional flows operators have hit ----------------------------

    #[test]
    fn composes_double_consonant_finals() {
        // 닭 (다 + ㄺ) — the combined final ㄺ comes from ㄹ+ㄱ. Pin the
        // two-step typing flow so a future combine_final tweak can't
        // silently break common Korean words.
        assert_eq!(committed("ㄷㅏㄹㄱ"), "닭");
        // 값 (가 + ㅄ)
        assert_eq!(committed("ㄱㅏㅂㅅ"), "값");
        // 앉 (안 + ㅈ → ㄵ)
        assert_eq!(committed("ㅇㅏㄴㅈ"), "앉");
    }

    #[test]
    fn composes_complex_vowels() {
        // 와 (오 + ㅏ → ㅘ) starts as a Pending::Vowel and lands as a
        // combined Pending::Vowel before getting attached to ㅇ. The
        // existing test covers it; this version proves the same code
        // works with the syllable path where the initial ㅇ is consumed
        // first.
        assert_eq!(committed("ㅇㅗㅏ"), "와");
        // 위 (우 + ㅣ → ㅟ)
        assert_eq!(committed("ㅇㅜㅣ"), "위");
        // 외 (오 + ㅣ → ㅚ)
        assert_eq!(committed("ㅇㅗㅣ"), "외");
    }

    #[test]
    fn vowel_after_double_final_splits_correctly() {
        // 닭 + ㅣ ("ㄷㅏㄹㄱㅣ") should produce 달기 — the ㄱ from the
        // double final moves into the next syllable's initial.
        assert_eq!(committed("ㄷㅏㄹㄱㅣ"), "달기");
        // 값 + ㅣ ("ㄱㅏㅂㅅㅣ") → 갑시
        assert_eq!(committed("ㄱㅏㅂㅅㅣ"), "갑시");
    }

    #[test]
    fn non_jamo_chars_commit_pending_then_pass_through() {
        // Typing "ㄱㅏ" then "!" must commit 가 first, then send "!" as
        // its own commit — the composer doesn't swallow non-jamo input.
        assert_eq!(committed("ㄱㅏ!"), "가!");
        // Same for digits or ASCII letters.
        assert_eq!(committed("ㅇㅏ1"), "아1");
    }

    #[test]
    fn flushing_an_empty_composer_does_nothing() {
        // Operator presses Cmd+T (which flushes pending IME) when nothing
        // is pending — must be a no-op so we don't emit a spurious empty
        // Commit or Preedit.
        let mut composer = HangulComposer::default();
        assert!(composer.flush().is_empty());
    }

    #[test]
    fn flushing_pending_syllable_commits_and_clears() {
        // 가 in progress, then flush → one Commit("가") + one
        // Preedit(None) clearing the editor's preedit area.
        let mut composer = HangulComposer::default();
        composer.push_text("ㄱㅏ");
        let actions = composer.flush();
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            ComposerAction::Commit(text) => assert_eq!(text, "가"),
            other => panic!("expected Commit, got {other:?}"),
        }
        assert!(matches!(actions[1], ComposerAction::Preedit(None)));
        assert!(composer.is_empty(), "composer state must be reset");
    }

    #[test]
    fn fresh_composer_is_empty() {
        // Defensive: a brand-new composer reports is_empty == true so
        // the editor doesn't think there's a stale preedit to render.
        assert!(HangulComposer::default().is_empty());
    }
}
