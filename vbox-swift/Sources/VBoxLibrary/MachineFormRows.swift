import AppKit
import SwiftUI

// SSH credential input rows shared by AddRemoteSheet and MachineConfigSheet.
// Both sheets follow the same visual language (caption label, roundedBorder
// field, caption2 hint). Add new shared rows here when another sheet needs
// the same shape.

@ViewBuilder
func identityFileRow(text: Binding<String>, hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        Text(L("Field identity")).font(.caption).foregroundStyle(.secondary)
        HStack(spacing: 6) {
            TextField(L("Field identity placeholder"), text: text)
                .textFieldStyle(.roundedBorder)
            Button(L("Field identity browse")) {
                let panel = NSOpenPanel()
                panel.canChooseDirectories = false
                panel.canChooseFiles = true
                panel.allowsMultipleSelection = false
                panel.showsHiddenFiles = true
                panel.directoryURL = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".ssh")
                if panel.runModal() == .OK, let url = panel.url {
                    text.wrappedValue = url.path
                }
            }
            .buttonStyle(.bordered)
        }
        Text(hint).font(.caption2).foregroundStyle(.secondary)
    }
}

// For AddRemoteSheet: first-time registration — only placeholder is shown,
// no keychain-status badge.
@ViewBuilder
func passwordRow(text: Binding<String>, placeholder: String, hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        Text(L("Field password")).font(.caption).foregroundStyle(.secondary)
        SecureField(placeholder, text: text)
            .textFieldStyle(.roundedBorder)
        Text(hint).font(.caption2).foregroundStyle(.secondary)
    }
}

// For MachineConfigSheet: editing — shows a badge for an existing
// keychain entry plus a Clear toggle.
@ViewBuilder
func passwordChangeRow(text: Binding<String>, hasExisting: Bool,
                       clearRequested: Binding<Bool>, hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        HStack(spacing: 8) {
            Text(L("Field password")).font(.caption).foregroundStyle(.secondary)
            if hasExisting && !clearRequested.wrappedValue {
                Text("• \(L("Field password current"))")
                    .font(.caption2)
                    .foregroundStyle(.tint)
            }
            Spacer()
            if hasExisting {
                Button(clearRequested.wrappedValue ? L("Cancel") : L("Field password clear")) {
                    clearRequested.wrappedValue.toggle()
                    if clearRequested.wrappedValue { text.wrappedValue = "" }
                }
                .buttonStyle(.borderless)
                .font(.caption2)
            }
        }
        SecureField(hasExisting ? L("Field password change placeholder")
                                : L("Field password placeholder"),
                    text: text)
            .textFieldStyle(.roundedBorder)
            .disabled(clearRequested.wrappedValue)
        Text(hint).font(.caption2).foregroundStyle(.secondary)
    }
}
