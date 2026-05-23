import AppKit
import SwiftUI

// AddRemoteSheet 와 MachineConfigSheet 가 공유하는 SSH 인증 입력 row.
// 두 시트 모두 같은 시각 언어 (caption label, roundedBorder field, caption2 hint)를
// 따른다. 새 시트가 같은 row 가 필요해지면 여기에 추가.

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

// AddRemoteSheet 용: 처음 등록 — placeholder 만 보이고 키체인 상태 표시는 없음.
@ViewBuilder
func passwordRow(text: Binding<String>, placeholder: String, hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        Text(L("Field password")).font(.caption).foregroundStyle(.secondary)
        SecureField(placeholder, text: text)
            .textFieldStyle(.roundedBorder)
        Text(hint).font(.caption2).foregroundStyle(.secondary)
    }
}

// MachineConfigSheet 용: 변경 — 기존 키체인 저장 여부 배지 + Clear 토글.
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
