import SwiftUI

// ───────────────────────────────────────────────────────────────────────────
// MARK: - SettingsPopover — global settings panel shown from the toolbar gear
//
// Single responsibility: a view composer that assembles each global setting as
// one row. Real state ownership is delegated outward:
//   * hostChromeEnabled  → @AppStorage binding owned by LibraryWindow
//   * language selection → LanguagePickerRow + LocalizationStore
//                          (injected via EnvironmentObject)
//
// New global settings should follow the same pattern: one Setting_*Row file
// plus one line here.
// ───────────────────────────────────────────────────────────────────────────

struct SettingsPopover: View {
    @Binding var hostChromeEnabled: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label(L("Global settings"), systemImage: "gearshape")
                .font(.headline)
                .labelStyle(.titleAndIcon)

            LanguagePickerRow()

            Divider()

            VStack(alignment: .leading, spacing: 6) {
                Toggle(isOn: $hostChromeEnabled) {
                    Label(
                        L("Show macOS titlebar"),
                        systemImage: hostChromeEnabled ? "macwindow" : "rectangle.dashed"
                    )
                }
                .toggleStyle(.switch)
                Text(L("Titlebar hint"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(L("Settings reapply hint"))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(16)
        .frame(width: 320)
    }
}
