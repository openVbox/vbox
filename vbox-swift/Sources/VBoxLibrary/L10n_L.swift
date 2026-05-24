import Foundation

// ───────────────────────────────────────────────────────────────────────────
// MARK: - L() — global translation lookup function
//
// Single responsibility: take a key (and optional arguments) and return the
// display string for the current language.
//
// Calling contract:
//  * nonisolated — safe to call from any thread or actor.
//    LocalizationStore.currentLanguage is an NSLock-protected atomic cache,
//    which is what allows L() to be invoked from inside stdout streaming
//    callbacks (arbitrary threads).
//  * Missing keys fall back to the en dictionary, then to the key itself.
//  * When the user changes language via the picker, LocalizationStore's
//    `selected` didSet refreshes the cache, so the next L() call sees the
//    new language. SwiftUI bodies re-evaluate automatically because
//    @EnvironmentObject receives the objectWillChange signal.
// ───────────────────────────────────────────────────────────────────────────

/// The currently active language dictionary. The master (en) is returned as
/// nil because it serves as the fallback handled separately at the end.
private func currentTable() -> [String: String]? {
    switch LocalizationStore.currentLanguage {
    case .en: return nil
    case .ko: return L10n.ko
    case .zh: return L10n.zh
    case .ja: return L10n.ja
    case .es: return L10n.es
    }
}

/// Short helper: `L("Machines")` → display string for the current language.
/// Missing keys fall back to en, then to the key itself.
func L(_ key: String) -> String {
    if let table = currentTable(), let v = table[key] { return v }
    return L10n.en[key] ?? key
}

/// Single-argument interpolation: `L("Active label", arg: name)` — replaces
/// the "{0}" placeholder in the dictionary value.
func L(_ key: String, arg: String) -> String {
    L(key).replacingOccurrences(of: "{0}", with: arg)
}

func L(_ key: String, _ a: String, _ b: String) -> String {
    L(key).replacingOccurrences(of: "{0}", with: a).replacingOccurrences(of: "{1}", with: b)
}

func L(_ key: String, _ a: String, _ b: String, _ c: String) -> String {
    L(key)
        .replacingOccurrences(of: "{0}", with: a)
        .replacingOccurrences(of: "{1}", with: b)
        .replacingOccurrences(of: "{2}", with: c)
}
