import Combine
import Foundation

// ───────────────────────────────────────────────────────────────────────────
// MARK: - LocalizationStore — current language state + UserDefaults persistence
//
// Single responsibility:
//  1) Reconcile the user's explicit choice (`selected`) with the system-default
//     guess (`systemDefault`) into the language to display right now.
//  2) On `selected` change, immediately mirror to UserDefaults so the choice
//     survives across launches.
//  3) Expose state as ObservableObject so SwiftUI views re-render reactively.
//  4) Expose `currentLanguage` as a nonisolated atomic, so the global L()
//     function can read it safely from any thread or actor.
//
// External deps: Foundation / Combine only. Knows nothing about SwiftUI or
// View code.
// ───────────────────────────────────────────────────────────────────────────

@MainActor
final class LocalizationStore: ObservableObject {
    /// Single shared instance for the whole app. Referenced by the global
    /// L() function and also injected into the SwiftUI view tree via
    /// environmentObject so views get change notifications.
    static let shared = LocalizationStore()

    /// The user's explicit choice. nil means follow the system default.
    /// @Published, so every subscriber (views, objectWillChange) refreshes
    /// on change.
    @Published var selected: AppLanguage? {
        didSet {
            persist(selected)
            Self.refreshCurrentCache(from: selected)
        }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let stored: AppLanguage?
        if let raw = defaults.string(forKey: L10nDefaults.languageKey),
           let lang = AppLanguage(rawValue: raw) {
            stored = lang
        } else {
            stored = nil
        }
        self.selected = stored
        Self.refreshCurrentCache(from: stored)
    }

    /// Convenience property for callers on the main actor; purely for UI
    /// code readability.
    var current: AppLanguage { Self.currentLanguage }

    // MARK: - Nonisolated cache (for the global L() function)
    //
    // `currentLanguage` is intentionally split out from LocalizationStore's
    // MainActor isolation so the global L() function can be invoked safely
    // from arbitrary threads (e.g. stdout streaming callbacks). Writes happen
    // only from `selected.didSet`, which runs on the main actor, but reads
    // are allowed from anywhere.

    /// Current-language cache that can be read safely from any thread or
    /// actor.
    nonisolated static var currentLanguage: AppLanguage {
        cacheLock.lock(); defer { cacheLock.unlock() }
        return _cachedCurrent
    }

    nonisolated private static func refreshCurrentCache(from selected: AppLanguage?) {
        let resolved = selected ?? AppLanguage.systemDefault
        cacheLock.lock()
        _cachedCurrent = resolved
        cacheLock.unlock()
    }

    nonisolated(unsafe) private static var _cachedCurrent: AppLanguage = AppLanguage.systemDefault
    nonisolated private static let cacheLock = NSLock()

    // MARK: - Private

    private let defaults: UserDefaults

    private func persist(_ value: AppLanguage?) {
        if let value {
            defaults.set(value.rawValue, forKey: L10nDefaults.languageKey)
        } else {
            defaults.removeObject(forKey: L10nDefaults.languageKey)
        }
    }
}
