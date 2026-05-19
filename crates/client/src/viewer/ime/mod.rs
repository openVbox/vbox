//! Multilingual input composers.
//!
//! Some host IMEs hand the application *decomposed* text (Hangul compatibility
//! jamo, Kana base + voicing mark, …) and expect the application to assemble
//! the final glyphs. When we relay those keystrokes to a remote guest we have
//! to do that assembly ourselves; otherwise the guest sees a stream of
//! standalone components instead of words.
//!
//! Each backend is a small state machine with the same shape:
//! `push_text`, `flush`, `backspace`, `is_empty`, plus a static `can_compose`
//! predicate used to detect which backend (if any) should handle a chunk of
//! incoming text. [`InputComposer`] is the dispatcher the rest of the client
//! talks to — it picks a backend based on the first composable chunk and
//! flushes/switches transparently when the input shifts to another script.

pub mod emoji;
mod hangul;
mod kana;

pub use hangul::HangulComposer;
pub use kana::KanaComposer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    Commit(String),
    Preedit(Option<String>),
}

/// True when at least one backend wants to assemble this text rather than
/// commit it verbatim. Callers use this to decide whether to route incoming
/// IME text through [`InputComposer`] or forward it directly.
pub fn can_compose(text: &str) -> bool {
    HangulComposer::can_compose(text) || KanaComposer::can_compose(text)
}

#[derive(Debug, Default)]
pub struct InputComposer {
    backend: Backend,
}

#[derive(Debug, Default)]
enum Backend {
    #[default]
    Idle,
    Hangul(HangulComposer),
    Kana(KanaComposer),
}

impl InputComposer {
    pub fn is_empty(&self) -> bool {
        match &self.backend {
            Backend::Idle => true,
            Backend::Hangul(c) => c.is_empty(),
            Backend::Kana(c) => c.is_empty(),
        }
    }

    pub fn push_text(&mut self, text: &str) -> Vec<ComposerAction> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut actions = self.switch_for(text);
        match &mut self.backend {
            Backend::Hangul(c) => actions.extend(c.push_text(text)),
            Backend::Kana(c) => actions.extend(c.push_text(text)),
            Backend::Idle => actions.push(ComposerAction::Commit(text.to_string())),
        }
        actions
    }

    pub fn flush(&mut self) -> Vec<ComposerAction> {
        let actions = match &mut self.backend {
            Backend::Idle => Vec::new(),
            Backend::Hangul(c) => c.flush(),
            Backend::Kana(c) => c.flush(),
        };
        self.backend = Backend::Idle;
        actions
    }

    pub fn backspace(&mut self) -> Vec<ComposerAction> {
        match &mut self.backend {
            Backend::Idle => Vec::new(),
            Backend::Hangul(c) => c.backspace(),
            Backend::Kana(c) => c.backspace(),
        }
    }

    fn switch_for(&mut self, text: &str) -> Vec<ComposerAction> {
        let target = detect(text);
        if self.backend_matches(target) {
            return Vec::new();
        }
        let actions = self.flush();
        self.backend = match target {
            Target::None => Backend::Idle,
            Target::Hangul => Backend::Hangul(HangulComposer::default()),
            Target::Kana => Backend::Kana(KanaComposer::default()),
        };
        actions
    }

    fn backend_matches(&self, target: Target) -> bool {
        matches!(
            (&self.backend, target),
            (Backend::Idle, Target::None)
                | (Backend::Hangul(_), Target::Hangul)
                | (Backend::Kana(_), Target::Kana)
        )
    }
}

#[derive(Clone, Copy)]
enum Target {
    None,
    Hangul,
    Kana,
}

fn detect(text: &str) -> Target {
    if HangulComposer::can_compose(text) {
        Target::Hangul
    } else if KanaComposer::can_compose(text) {
        Target::Kana
    } else {
        Target::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_commits(actions: Vec<ComposerAction>) -> String {
        actions
            .into_iter()
            .filter_map(|a| match a {
                ComposerAction::Commit(text) => Some(text),
                ComposerAction::Preedit(_) => None,
            })
            .collect()
    }

    fn drive(composer: &mut InputComposer, input: &str) -> String {
        let mut out = collect_commits(composer.push_text(input));
        out.push_str(&collect_commits(composer.flush()));
        out
    }

    #[test]
    fn dispatches_to_hangul_backend() {
        let mut composer = InputComposer::default();
        assert_eq!(drive(&mut composer, "ㅎㅏㄴㄱㅡㄹ"), "한글");
    }

    #[test]
    fn dispatches_to_kana_backend() {
        let mut composer = InputComposer::default();
        assert_eq!(drive(&mut composer, "か\u{3099}"), "が");
    }

    #[test]
    fn switching_backends_flushes_previous() {
        let mut composer = InputComposer::default();
        let mut out = collect_commits(composer.push_text("ㄱㅏ"));
        out.push_str(&collect_commits(composer.push_text("は\u{3099}")));
        out.push_str(&collect_commits(composer.flush()));
        assert_eq!(out, "가ば");
    }

    #[test]
    fn can_compose_detection() {
        assert!(can_compose("ㄱㅏ"));
        assert!(can_compose("は\u{3099}"));
        assert!(!can_compose("hello"));
        assert!(!can_compose("한글"));
    }

    #[test]
    fn is_empty_tracks_backend_state() {
        let mut composer = InputComposer::default();
        assert!(composer.is_empty());

        composer.push_text("ㄱ");
        assert!(
            !composer.is_empty(),
            "Hangul backend should hold pending jamo"
        );

        composer.flush();
        assert!(composer.is_empty(), "flush should drain pending state");

        composer.push_text("か\u{3099}");
        assert!(
            composer.is_empty(),
            "Kana backend has nothing pending after a complete pair"
        );
    }

    #[test]
    fn backspace_routes_to_active_backend() {
        let mut composer = InputComposer::default();
        assert!(
            composer.backspace().is_empty(),
            "idle backspace must produce no actions"
        );

        composer.push_text("ㄱ");
        let actions = composer.backspace();
        assert_eq!(actions, vec![ComposerAction::Preedit(None)]);
        assert!(composer.is_empty());
    }

    #[test]
    fn same_backend_chunks_do_not_flush() {
        let mut composer = InputComposer::default();
        let mut out = collect_commits(composer.push_text("ㄱ"));
        // A second Hangul chunk must keep the backend so 가 composes across calls.
        out.push_str(&collect_commits(composer.push_text("ㅏ")));
        out.push_str(&collect_commits(composer.flush()));
        assert_eq!(out, "가");
    }
}
