import SwiftUI

// ───────────────────────────────────────────────────────────────────────────
// MARK: - LanguagePickerRow — language selector row for the settings popover
//
// Single responsibility: a picker component that lets the user choose the
// display language. Selection changes flow into LocalizationStore.selected
// immediately, and the store's didSet handles UserDefaults persistence and
// nonisolated-cache refresh.
//
// External deps: LocalizationStore (EnvironmentObject), AppLanguage, L() only.
// Reusable as-is in other surfaces (e.g. a first-run wizard), not only in
// SettingsPopover.
// ───────────────────────────────────────────────────────────────────────────

struct LanguagePickerRow: View {
    @EnvironmentObject private var l10n: LocalizationStore

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker(selection: selectionBinding) {
                // nil = system default. The tag is matched as nil too.
                Text(L("System default")).tag(AppLanguage?.none)
                Divider()
                ForEach(AppLanguage.allCases) { lang in
                    Text(lang.endonym).tag(AppLanguage?.some(lang))
                }
            } label: {
                Label(L("Language"), systemImage: "globe")
            }
            .pickerStyle(.menu)
        }
    }

    /// Direct binding to `l10n.selected`. The wrapped value is forwarded
    /// as-is so the didSet observer fires.
    private var selectionBinding: Binding<AppLanguage?> {
        Binding(
            get: { l10n.selected },
            set: { l10n.selected = $0 }
        )
    }
}
