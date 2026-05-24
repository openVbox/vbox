import Foundation

// ───────────────────────────────────────────────────────────────────────────
// MARK: - AppLanguage — supported-language enum + endonym helper
//
// Single responsibility: represent supported language codes and provide both
// system-Locale-based detection and a user-facing endonym (the language's own
// name). UserDefaults access, ObservableObject, and SwiftUI dependencies all
// live in other files.
// ───────────────────────────────────────────────────────────────────────────

enum AppLanguage: String, CaseIterable, Identifiable {
    case en, ko, zh, ja, es

    var id: String { rawValue }

    /// Inferred from the first entry in the system's preferred languages.
    /// Used as the fallback when the user has not made a manual selection
    /// (i.e. "System default").
    static var systemDefault: AppLanguage {
        let primary = Locale.preferredLanguages.first ?? "en"
        if primary.hasPrefix("ko") { return .ko }
        if primary.hasPrefix("zh") { return .zh }
        if primary.hasPrefix("ja") { return .ja }
        if primary.hasPrefix("es") { return .es }
        return .en
    }

    /// The language's name in its own script. Used verbatim as the Picker
    /// label. Spec: every dictionary uses the same endonym, so it is defined
    /// here in one place instead of in the translation dictionaries.
    var endonym: String {
        switch self {
        case .en: return "English"
        case .ko: return "한국어"
        case .ja: return "日本語"
        case .zh: return "简体中文"
        case .es: return "Español"
        }
    }
}

/// Single source of truth for UserDefaults keys, so other modules do not
/// hard-code the strings themselves.
enum L10nDefaults {
    /// nil = system default; "en"/"ko"/"ja"/"zh"/"es" = explicit user choice.
    static let languageKey = "vbox.app.language"
}
