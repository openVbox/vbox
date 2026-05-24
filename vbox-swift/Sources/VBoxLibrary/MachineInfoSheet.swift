import AppKit
import SwiftUI

// 1:1 Swift mirror of the CLI's `vbox machines info <target> --json` output.
// When a new field is added to the CLI's MachineRecord, mirror it here too.
// Every overrides value is a String because set_cmd stores it as
// `Value::String` only.
struct MachineDetails: Decodable {
    let record: Record
    let overrides: [String: String]?
    let probe: ProbeReport?

    struct Record: Decodable {
        let uuid: String
        let name: String
        let status: String
        let ip: String
        let osRaw: String
        let osKind: String
        let sshUser: String
        let sshHost: String
        let guestDir: String
        let identityFile: String
        let hasPassword: Bool

        enum CodingKeys: String, CodingKey {
            case uuid, name, status, ip
            case osRaw = "os_raw"
            case osKind = "os_kind"
            case sshUser = "ssh_user"
            case sshHost = "ssh_host"
            case guestDir = "guest_dir"
            case identityFile = "identity_file"
            case hasPassword = "has_password"
        }
    }

    struct ProbeReport: Decodable {
        let ssh: SshReport
        let guest: GuestReport?
        let probedAt: String

        struct SshReport: Decodable {
            let reachable: Bool
            let rttMs: Int?
            let error: String?

            enum CodingKeys: String, CodingKey {
                case reachable, error
                case rttMs = "rtt_ms"
            }
        }

        struct GuestReport: Decodable {
            let uptime: String?
        }

        enum CodingKeys: String, CodingKey {
            case ssh, guest
            case probedAt = "probed_at"
        }
    }
}

extension MachinesModel {
    func loadDetails(for machine: GuestMachine) async -> MachineDetails? {
        let result = await VBoxRunner.run(
            ["machines", "info", machine.uuid, "--json"],
            config: config, override: machine)
        guard result.status == 0 else { return nil }
        return try? JSONDecoder().decode(MachineDetails.self,
                                          from: Data(result.output.utf8))
    }

    // A probe can include an SSH handshake plus ConnectTimeout=3, so it is
    // expensive. Reopening the sheet for the same machine within 5 minutes
    // returns the cached result immediately.
    // Pass `force` to bypass the cache and re-measure (Re-probe button).
    func probeDetails(for machine: GuestMachine,
                      force: Bool = false) async -> MachineDetails? {
        let key = machine.uuid
        if !force, let cached = await ProbeCache.shared.get(key) {
            return cached
        }
        let result = await VBoxRunner.run(
            ["machines", "info", machine.uuid, "--json", "--probe"],
            config: config, override: machine)
        guard result.status == 0,
              let details = try? JSONDecoder().decode(MachineDetails.self,
                                                      from: Data(result.output.utf8))
        else { return nil }
        await ProbeCache.shared.put(key, details)
        return details
    }
}

// In-memory probe cache. Implemented as an actor for thread safety.
fileprivate actor ProbeCache {
    static let shared = ProbeCache()
    private var entries: [String: (Date, MachineDetails)] = [:]
    private let ttl: TimeInterval = 300  // 5 minutes

    func get(_ key: String) -> MachineDetails? {
        guard let entry = entries[key],
              Date().timeIntervalSince(entry.0) < ttl else { return nil }
        return entry.1
    }

    func put(_ key: String, _ details: MachineDetails) {
        entries[key] = (Date(), details)
    }
}

// Read-only machine details sheet. No editing controls; loosely follows the
// "labeled value" style from MachineConfigSheet (caption label + monospaced
// value).
struct MachineInfoSheet: View {
    let machine: GuestMachine
    @ObservedObject var model: MachinesModel
    @Environment(\.dismiss) private var dismiss

    @State private var details: MachineDetails?
    @State private var loadFailed = false
    @State private var isLoading = true
    @State private var probeResult: MachineDetails.ProbeReport?
    @State private var isProbing = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            Divider()
            content
            footer
        }
        .padding(20)
        .frame(minWidth: 480, idealWidth: 540, minHeight: 420, idealHeight: 500)
        .task {
            details = await model.loadDetails(for: machine)
            loadFailed = (details == nil)
            isLoading = false
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "info.circle").font(.title2).foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(L("Machine info")).font(.headline)
                Text(machine.name).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
        }
    }

    @ViewBuilder
    private var content: some View {
        if isLoading {
            ProgressView(L("Loading"))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let d = details {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    section(L("Identity section")) {
                        row("uuid", d.record.uuid)
                        row("name", d.record.name)
                        row("status", d.record.status)
                        row("ip", d.record.ip)
                        row("os", "\(d.record.osRaw) (\(d.record.osKind))")
                    }
                    section(L("SSH section")) {
                        row("ssh_user", d.record.sshUser)
                        row("ssh_host", d.record.sshHost)
                        if !d.record.identityFile.isEmpty {
                            row("identity_file", d.record.identityFile)
                        }
                        if d.record.hasPassword {
                            row("password", L("Field password current"))
                        }
                    }
                    section(L("Guest path section")) {
                        row("guest_dir", d.record.guestDir)
                    }
                    if let extras = d.overrides, !extras.isEmpty {
                        section(L("Custom overrides section")) {
                            ForEach(extras.sorted(by: { $0.key < $1.key }),
                                    id: \.key) { kv in
                                row(kv.key, kv.value)
                            }
                        }
                    }
                    section(L("Probe section")) {
                        probeContent(initial: d.probe)
                    }
                }
                .padding(.vertical, 4)
            }
        } else if loadFailed {
            Text(L("Failed to load info"))
                .foregroundStyle(.red).font(.callout)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var footer: some View {
        HStack {
            Spacer()
            Button(L("Close")) { dismiss() }.keyboardShortcut(.cancelAction)
        }
    }

    @ViewBuilder
    private func section<Content: View>(_ title: String,
                                        @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 4) { content() }
                .padding(10)
                .background(Color(NSColor.controlBackgroundColor))
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
    }

    @ViewBuilder
    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 100, alignment: .leading)
            Text(value)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private func probeContent(initial: MachineDetails.ProbeReport?) -> some View {
        // probeResult is populated after the user explicitly probes. `initial`
        // is always nil because the first load runs without --probe, but it is
        // accepted here as a fallback for forward compatibility.
        if let p = probeResult ?? initial {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Image(systemName: p.ssh.reachable
                          ? "checkmark.circle.fill"
                          : "xmark.circle.fill")
                        .foregroundStyle(p.ssh.reachable ? .green : .red)
                        .font(.caption)
                    Text(p.ssh.reachable ? L("Reachable") : L("Unreachable"))
                        .font(.caption.weight(.semibold))
                }
                if let rtt = p.ssh.rttMs {
                    row("ssh.rtt_ms", "\(rtt)")
                }
                if let err = p.ssh.error {
                    row("ssh.error", err)
                }
                if let uptime = p.guest?.uptime {
                    row("uptime", uptime)
                }
                row("probed_at", p.probedAt)
                HStack {
                    Spacer()
                    Button(L("Re-probe")) {
                        Task { await runProbe(force: true) }
                    }
                    .controlSize(.small)
                    .disabled(isProbing)
                }
            }
        } else if isProbing {
            HStack {
                ProgressView().controlSize(.small)
                Text(L("Probing")).font(.caption).foregroundStyle(.secondary)
            }
        } else {
            HStack {
                Spacer()
                Button(L("Run probe")) {
                    Task { await runProbe(force: false) }
                }
                .controlSize(.small)
            }
        }
    }

    private func runProbe(force: Bool) async {
        isProbing = true
        if let result = await model.probeDetails(for: machine, force: force) {
            probeResult = result.probe
        }
        isProbing = false
    }
}
