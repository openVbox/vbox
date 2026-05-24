import AppKit
import Combine
import SwiftUI

enum VBoxSwiftVersion {
    static let current = "0.1.3"
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - L10n
// All localization code has moved into L10n_*.swift:
//   * L10n_AppLanguage.swift     — supported-language enum + endonym helper
//   * L10n_Store.swift           — LocalizationStore (UserDefaults persistence + ObservableObject)
//   * L10n_L.swift               — global L(_:) / L(_:, arg:) ... functions
//   * L10n_Translations.swift    — en/ko/zh/ja/es translation dictionaries
// ───────────────────────────────────────────────────────────────────────────


struct AppConfig: Sendable {
    let root: String
    let cliPath: String
    let stateDir: String
    let launcherDir: String
    let iconCacheDir: String
    let distroIconDir: String
    let guest: String
    let guestDir: String
    let port: String
    let socket: String
    let width: String
    let height: String
    let suffix: String

    static func load() -> AppConfig {
        AppConfig(
            root: readResource("Root", fallback: ""),
            cliPath: readResource("CliPath", fallback: ""),
            stateDir: readResource("StateDir", fallback: ""),
            launcherDir: readResource("LauncherDir", fallback: ""),
            iconCacheDir: readResource("IconCacheDir", fallback: ""),
            distroIconDir: readResource("DistroIconDir", fallback: ""),
            guest: readResource("Guest", fallback: ""),
            guestDir: readResource("GuestDir", fallback: ""),
            port: readResource("Port", fallback: "5710"),
            socket: readResource("Socket", fallback: "vbox-0"),
            width: readResource("Width", fallback: "1024"),
            height: readResource("Height", fallback: "768"),
            suffix: readResource("Suffix", fallback: "")
        )
    }

    private static func readResource(_ name: String, fallback: String) -> String {
        guard let url = Bundle.main.url(forResource: name, withExtension: "txt"),
              let value = try? String(contentsOf: url, encoding: .utf8) else {
            return fallback
        }
        return value.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

struct GuestApp: Identifiable, Equatable {
    let id: String
    let name: String
    let execLine: String
    let icon: String
    let desktop: String
    let categories: String
    let argvB64: String
    var installed: Bool
}

struct RunningProcess: Identifiable, Equatable, Hashable {
    let pid: Int32
    let name: String
    let command: String
    let startedAt: Date

    var id: Int32 { pid }
}

struct CommandResult: Sendable {
    let status: Int32
    let output: String
}

@MainActor
final class LibraryModel: ObservableObject {
    @Published var apps: [GuestApp] = []
    @Published var search = ""
    @Published var selectedID: String?
    @Published var isWorking = false
    @Published var status = ""
    // Progress of the bulk Launchpad-bundle install. value 0..1; nil means
    // indeterminate or not in progress.
    @Published var bundleProgress: Double? = nil
    @Published var bundleProgressLabel: String = ""
    @Published var runningProcesses: [RunningProcess] = []
    @Published var selectedRunningPID: Int32?
    @Published var isRefreshingProcesses = false
    @Published var processesStatus = ""

    // Active guest override — when MachinesModel sets this, every external
    // vbox invocation uses that guest's ssh user/host/dir. nil falls back to
    // the AppConfig defaults.
    @Published var activeGuestOverride: GuestMachine? = nil

    private var pollingTask: Task<Void, Never>?

    let config = AppConfig.load()

    // SSH string of the currently active guest (e.g. "pista@10.211.55.11"),
    // for display only.
    var activeGuestString: String {
        if let g = activeGuestOverride { return g.sshString }
        return config.guest
    }

    var filteredApps: [GuestApp] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !query.isEmpty else { return apps }
        return apps.filter { app in
            "\(app.name) \(app.id) \(app.categories)".lowercased().contains(query)
        }
    }

    var selectedApp: GuestApp? {
        if let selectedID, let app = apps.first(where: { $0.id == selectedID }) {
            return app
        }
        return filteredApps.first
    }

    init() {
        reload()
    }

    func reload() {
        let loaded = loadApps()
        apps = loaded
        if let selectedID, loaded.contains(where: { $0.id == selectedID }) {
            return
        }
        selectedID = loaded.first?.id
    }

    func refresh() {
        guard !isWorking else { return }
        isWorking = true
        status = L("Refresh")
        Task {
            let result = await runVBoxNative(["cache-icons", "--refresh"])
            reload()
            status = result.status == 0 ? L("Library fresh") : trimmedOutput(result.output, fallback: L("Refresh failed"))
            isWorking = false
        }
    }

    func loadLibrary() {
        guard !isWorking else { return }
        isWorking = true
        status = L("Reload library")
        Task {
            // Step 1: refresh the guest app library so newly-installed GNOME
            // apps show up.
            let refresh = await runVBoxNative(["library", "--refresh"])
            if refresh.status != 0 {
                reload()
                status = trimmedOutput(refresh.output, fallback: L("Refresh failed"))
                isWorking = false
                return
            }
            // A fresh cache just arrived, so reload up front — we also need
            // the count to use as the denominator (total) for install progress.
            reload()
            let total = apps.count
            // Step 2: invoke install-apps in streaming mode to surface progress.
            //   The bash side emits one `[vbox] installed: <name> -> <path>`
            //   line per app on stdout.
            //   The per-line callback increments a done counter and updates
            //   bundleProgress.
            status = L("Launchpad bundles refresh")
            bundleProgress = 0
            bundleProgressLabel = "0 / \(total)"
            let cfg = config
            let override = activeGuestOverride
            var doneCount = 0
            let install = await VBoxRunner.runStreaming(["install-apps"], config: cfg, override: override) { line in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    if line.hasPrefix("[vbox] installed:") {
                        // "[vbox] installed: <name> -> <path>"
                        let after = line.replacingOccurrences(of: "[vbox] installed:", with: "")
                        let name = after.components(separatedBy: " -> ").first?
                            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                        doneCount += 1
                        if total > 0 {
                            self.bundleProgress = min(1.0, Double(doneCount) / Double(total))
                        }
                        self.bundleProgressLabel = total > 0
                            ? "\(doneCount) / \(total)"
                            : L("Apps count {0}", arg: "\(doneCount)")
                        self.status = L("Bundle progress phase with name {0} {1} {2}", "\(doneCount)", "\(max(total, doneCount))", name)
                    } else if line.hasPrefix("[vbox] ") {
                        // Forward preliminary messages (build/sync, etc.)
                        // into the status line as well.
                        // e.g. "[vbox] build host client" → "build host client"
                        let trimmed = line.dropFirst("[vbox] ".count).trimmingCharacters(in: .whitespacesAndNewlines)
                        if !trimmed.isEmpty { self.status = trimmed }
                    }
                }
            }
            reload()
            bundleProgress = nil
            bundleProgressLabel = ""
            if install.status == 0 {
                status = L("Library reloaded {0}", arg: "\(apps.count)")
            } else {
                status = trimmedOutput(install.output, fallback: L("Save failed"))
            }
            isWorking = false
        }
    }

    // Default behavior: launch from the bundle if installed, otherwise from
    // the viewer. (compatibility alias)
    func runSelected() {
        guard let app = selectedApp else { return }
        if app.installed {
            runAsBundle(app)
        } else {
            runInViewer(app)
        }
    }

    // (1) Run inside the viewer — reuse an open viewer if one exists,
    //     otherwise spawn a new viewer.
    //     Every guest app lives as a nested toplevel inside a single macOS
    //     window (winit + softbuffer).
    func runInViewer(_ app: GuestApp) {
        guard !isWorking else { return }
        isWorking = true
        status = L("Viewer started {0}", arg: app.name)
        Task {
            let result = await runVBoxNative(["launch-id", app.id])
            if result.status != 0 {
                status = trimmedOutput(result.output, fallback: L("Run failed"))
            } else {
                try? await Task.sleep(nanoseconds: 800_000_000)
                await refreshProcesses(silent: true)
            }
            isWorking = false
        }
    }

    // (2) Run as an app bundle — launch ~/Applications/vbox/<App>.app as a
    //     standalone macOS app.
    //     Shows up in Launchpad/Dock with its own winit window. Requires the
    //     bundle to be created beforehand via install-apps.
    func runAsBundle(_ app: GuestApp) {
        guard !isWorking else { return }
        let bundlePath = launcherPath(for: app)
        guard FileManager.default.fileExists(atPath: bundlePath) else {
            status = L("Bundle missing")
            return
        }
        isWorking = true
        status = L("Bundle started {0}", arg: app.name)
        Task {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            process.arguments = ["-n", bundlePath]
            do {
                try process.run()
                process.waitUntilExit()
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                await refreshProcesses(silent: true)
            } catch {
                status = L("Bundle run failed {0}", arg: error.localizedDescription)
            }
            isWorking = false
        }
    }

    var selectedProcess: RunningProcess? {
        guard let selectedRunningPID else { return runningProcesses.first }
        return runningProcesses.first { $0.pid == selectedRunningPID }
    }

    func refreshProcesses(silent: Bool = false) async {
        if !silent {
            isRefreshingProcesses = true
        }
        let result = await runVBoxNative(["processes"])
        if result.status == 0 {
            let parsed = parseProcesses(result.output)
            runningProcesses = parsed
            if let selectedRunningPID, !parsed.contains(where: { $0.pid == selectedRunningPID }) {
                self.selectedRunningPID = parsed.first?.pid
            } else if selectedRunningPID == nil {
                selectedRunningPID = parsed.first?.pid
            }
            processesStatus = parsed.isEmpty ? L("No running apps") : L("Running with count", arg: "\(parsed.count)")
        } else {
            processesStatus = trimmedOutput(result.output, fallback: L("Refresh failed"))
        }
        if !silent {
            isRefreshingProcesses = false
        }
    }

    func killProcess(_ process: RunningProcess) {
        guard !isRefreshingProcesses else { return }
        let pid = process.pid
        processesStatus = L("PID {0}", arg: "\(pid)") + " — " + L("End")
        Task {
            isRefreshingProcesses = true
            let result = await runVBoxNative(["kill-pid", String(pid)])
            if result.status != 0 {
                processesStatus = trimmedOutput(result.output, fallback: L("Run failed"))
            }
            await refreshProcesses(silent: true)
            isRefreshingProcesses = false
        }
    }

    func startPollingProcesses(interval: UInt64 = 2_000_000_000) {
        guard pollingTask == nil else { return }
        pollingTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshProcesses(silent: true)
                try? await Task.sleep(nanoseconds: interval)
            }
        }
    }

    func stopPollingProcesses() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    private func parseProcesses(_ text: String) -> [RunningProcess] {
        text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line -> RunningProcess? in
                let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
                guard parts.count >= 4, let pid = Int32(parts[0]) else { return nil }
                let started = Double(parts[1]).map { Date(timeIntervalSince1970: $0) } ?? Date()
                return RunningProcess(pid: pid, name: parts[2], command: parts[3], startedAt: started)
            }
            .sorted { lhs, rhs in
                if lhs.startedAt == rhs.startedAt { return lhs.pid < rhs.pid }
                return lhs.startedAt > rhs.startedAt
            }
    }

    func setLaunchpad(_ app: GuestApp, enabled: Bool) {
        guard !isWorking else { return }
        isWorking = true
        status = enabled ? "Launchpad" : "Launchpad"
        Task {
            let command = enabled ? "install-launcher" : "remove-launcher"
            let result = await runVBoxNative([command, app.id])
            reload()
            if result.status == 0 {
                status = enabled ? "Launchpad ✓" : "Launchpad ✗"
            } else {
                status = trimmedOutput(result.output, fallback: L("Save failed"))
            }
            isWorking = false
        }
    }

    func icon(for app: GuestApp) -> NSImage? {
        let launcherIcon = launcherPath(for: app).appending("/Contents/Resources/AppIcon.icns")
        if FileManager.default.fileExists(atPath: launcherIcon) {
            return NSImage(contentsOfFile: launcherIcon)
        }

        let safeID = sanitizeID(app.id)
        for dir in [activeIconDir, config.iconCacheDir] where !dir.isEmpty {
            for ext in ["png", "jpg", "jpeg", "tiff", "tif", "icns"] {
                let path = "\(dir)/\(safeID).\(ext)"
                if FileManager.default.fileExists(atPath: path), let image = NSImage(contentsOfFile: path) {
                    return image
                }
            }
        }
        return nil
    }

    // Cache dir for the active machine — a directory derived from
    // sanitizing VBOX_GUEST.
    // Same rules as bash sanitize_guest_id(): '@'→'_at_', other non
    // alnum/./-/_ → '_', and collapse consecutive '_'.
    private var activeMachineDir: String {
        "\(config.stateDir)/machines/\(LibraryModel.sanitizeGuestId(activeGuestString))"
    }

    private var activeIconDir: String { "\(activeMachineDir)/icons" }

    static func sanitizeGuestId(_ guest: String) -> String {
        let withAt = guest.replacingOccurrences(of: "@", with: "_at_")
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        var out = ""
        var prevUnderscore = false
        for sc in withAt.unicodeScalars {
            if allowed.contains(sc) {
                out.unicodeScalars.append(sc)
                prevUnderscore = false
            } else if !prevUnderscore {
                out.append("_")
                prevUnderscore = true
            }
        }
        return out
    }

    private func loadApps() -> [GuestApp] {
        let cachePath = "\(activeMachineDir)/app-library.tsv"
        let fallbackCachePath = "\(config.stateDir)/app-library.tsv"
        let effectiveCachePath = FileManager.default.fileExists(atPath: cachePath) ? cachePath : fallbackCachePath
        guard let text = try? String(contentsOfFile: effectiveCachePath, encoding: .utf8) else {
            return []
        }

        return text
            .split(separator: "\n", omittingEmptySubsequences: true)
            .compactMap { line in
                let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
                guard parts.count >= 7 else { return nil }
                let base = GuestApp(
                    id: parts[0],
                    name: parts[1],
                    execLine: parts[2],
                    icon: parts[3],
                    desktop: parts[4],
                    categories: parts[5],
                    argvB64: parts[6],
                    installed: false
                )
                var app = base
                app.installed = launcherInstalled(for: base)
                return app
            }
    }

    private func launcherInstalled(for app: GuestApp) -> Bool {
        let appPath = launcherPath(for: app)
        let macosPath = "\(appPath)/Contents/MacOS"
        let hasClient = (try? FileManager.default.contentsOfDirectory(atPath: macosPath))?
            .contains(where: { item in item.hasPrefix("vbox-client") }) ?? false
        return hasClient
            && FileManager.default.fileExists(atPath: "\(appPath)/Contents/Info.plist")
            && FileManager.default.fileExists(atPath: "\(appPath)/Contents/Resources/AppID.txt")
    }

    private func launcherPath(for app: GuestApp) -> String {
        let display: String
        if config.suffix.isEmpty {
            display = app.name
        } else {
            display = "\(app.name) (\(config.suffix))"
        }
        return "\(config.launcherDir)/\(safeAppFilename(display)).app"
    }

    private func safeAppFilename(_ value: String) -> String {
        value
            .replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: "-")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func sanitizeID(_ value: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        var result = ""
        var previousWasDash = false
        for scalar in value.unicodeScalars {
            if allowed.contains(scalar) {
                result.unicodeScalars.append(scalar)
                previousWasDash = false
            } else if !previousWasDash {
                result.append("-")
                previousWasDash = true
            }
        }
        return result.trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    }

    private func runVBoxNative(_ args: [String]) async -> CommandResult {
        let config = self.config
        let override = self.activeGuestOverride
        return await Task.detached(priority: .userInitiated) {
            await VBoxRunner.run(args, config: config, override: override)
        }.value
    }

    private func trimmedOutput(_ output: String, fallback: String) -> String {
        let text = output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return fallback }
        return String(text.prefix(180))
    }
}

// Single source of truth for cross-view UserDefaults keys used by this
// launcher. Keeping the literal strings here means a typo can't drift
// between LibraryWindow's @AppStorage and VBoxRunner's manual read.
enum VBoxDefaults {
    /// Mirrors the VBOX_HOST_CHROME env probe in the Rust viewer
    /// (see crates/client/src/viewer/env.rs::host_chrome_enabled).
    /// Default: true (standard macOS titlebar). VBoxRunner reads this
    /// key on every launch to inject VBOX_HOST_CHROME into the viewer
    /// child process.
    static let hostChromeKey = "vbox.hostChrome"
}

struct LibraryWindow: View {
    @StateObject private var model: LibraryModel
    @StateObject private var machinesModel: MachinesModel
    @State private var showRunning = false
    @State private var showMachines = false
    @State private var showSettings = false
    // Workaround for the machines-config sheet's nested-sheet restriction —
    // close the machines sheet first, then present this one.
    @State private var configMachine: GuestMachine? = nil
    @State private var infoMachine: GuestMachine? = nil
    @State private var showAddRemoteAtRoot = false
    @AppStorage(VBoxDefaults.hostChromeKey) private var hostChromeEnabled: Bool = true

    init() {
        let lib = LibraryModel()
        _model = StateObject(wrappedValue: lib)
        _machinesModel = StateObject(wrappedValue: MachinesModel(config: lib.config))
    }

    var body: some View {
        NavigationSplitView {
            appList
                .navigationSplitViewColumnWidth(min: 280, ideal: 330, max: 420)
        } detail: {
            detail
        }
        .searchable(text: $model.search, placement: .sidebar, prompt: L("Search"))
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Button {
                    model.refresh()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(model.isWorking)
                .help(L("Refresh icons"))
            }

            ToolbarItem(placement: .navigation) {
                Button {
                    model.loadLibrary()
                } label: {
                    Image(systemName: "square.and.arrow.down")
                }
                .disabled(model.isWorking)
                .help(L("Reload library + Launchpad"))
            }

            ToolbarItem(placement: .principal) {
                Button {
                    showMachines = true
                } label: {
                    Label(machinesButtonLabel, systemImage: "rectangle.3.group")
                }
                .help(L("Machines toolbar help"))
            }

            ToolbarItem(placement: .primaryAction) {
                Button {
                    showRunning = true
                } label: {
                    Label(runningButtonLabel, systemImage: "bolt.horizontal.circle")
                }
                .help(L("View running help"))
            }

            ToolbarItem(placement: .primaryAction) {
                Button {
                    showSettings.toggle()
                } label: {
                    Image(systemName: "gearshape")
                }
                .help(L("Global settings"))
                .popover(isPresented: $showSettings, arrowEdge: .bottom) {
                    SettingsPopover(hostChromeEnabled: $hostChromeEnabled)
                }
            }
            // The big "Run" button is already in the detail pane next to
            // the selected app; a second toolbar copy was redundant, so
            // it was removed.
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(.regularMaterial)
        .overlay(WindowConfigurator().allowsHitTesting(false))
        .sheet(isPresented: $showRunning) {
            RunningProcessesSheet(model: model)
                .frame(minWidth: 560, minHeight: 380)
        }
        .sheet(isPresented: $showMachines) {
            MachinesSheet(
                model: machinesModel,
                activeUUID: model.activeGuestOverride?.uuid,
                onConfigRequest: { machine in
                    showMachines = false
                    configMachine = machine
                },
                onAddRemoteRequest: {
                    showMachines = false
                    showAddRemoteAtRoot = true
                },
                onInfoRequest: { machine in
                    showMachines = false
                    infoMachine = machine
                })
        }
        .sheet(item: $configMachine) { machine in
            MachineConfigSheet(machine: machine, model: machinesModel)
        }
        .sheet(item: $infoMachine) { machine in
            MachineInfoSheet(machine: machine, model: machinesModel)
        }
        .sheet(isPresented: $showAddRemoteAtRoot) {
            AddRemoteSheet { name, ssh, dir, osRaw, identityFile, password in
                Task {
                    let ok = await machinesModel.addRemote(name: name, ssh: ssh,
                                                            guestDir: dir, osRaw: osRaw,
                                                            identityFile: identityFile,
                                                            password: password)
                    if ok { showAddRemoteAtRoot = false }
                }
            }
        }
        .task {
            machinesModel.onSelect = { machine in
                // When the selection changes, swap LibraryModel's override
                // and reload the new guest's app cache + processes. Each
                // machine has its own cache.
                model.activeGuestOverride = machine
                model.reload()
                Task { await model.refreshProcesses(silent: true) }
                // Surface the active machine + cache state in the status line.
                if let m = machine {
                    let count = model.apps.count
                    model.status = count > 0
                        ? L("Active machine with count {0} {1}", m.name, "\(count)")
                        : L("Active machine empty cache {0}", arg: m.name)
                } else {
                    model.status = L("Default guest active")
                }
            }
            model.startPollingProcesses()
            await machinesModel.refresh()
        }
        .onDisappear {
            model.stopPollingProcesses()
        }
    }

    private var machinesButtonLabel: String {
        let base: String
        if let g = model.activeGuestOverride {
            base = g.name
        } else {
            let host = model.config.guest.split(separator: "@").last.map(String.init) ?? model.config.guest
            base = host.isEmpty ? L("Machines") : host
        }
        let n = model.apps.count
        return n > 0 ? base + " · " + L("Apps count {0}", arg: "\(n)") : base
    }

    private var runningButtonLabel: String {
        let count = model.runningProcesses.count
        return count > 0 ? "실행 중 (\(count))" : L("Status running")
    }

    private var appList: some View {
        List(selection: $model.selectedID) {
            ForEach(model.filteredApps) { app in
                AppRow(app: app, image: model.icon(for: app))
                    .tag(Optional(app.id))
            }
        }
        .listStyle(.sidebar)
    }

    @ViewBuilder
    private var detail: some View {
        if let app = model.selectedApp {
            AppDetail(app: app, image: model.icon(for: app), model: model)
        } else {
            EmptyLibraryView(model: model)
        }
    }
}

// Detail area shown when a machine's cache is empty. Tells the user *why*
// it is empty and offers a single entry point to fetch that machine's
// library.
struct EmptyLibraryView: View {
    @ObservedObject var model: LibraryModel

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "square.grid.2x2")
                .font(.system(size: 44, weight: .semibold))
                .foregroundStyle(.secondary)
            Text(L("Library empty title"))
                .font(.title3.weight(.semibold))
            Text(L("Library empty subtitle", arg: model.activeGuestString))
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 360)
            Button {
                model.loadLibrary()
            } label: {
                Label(L("Fetch this machine's library"), systemImage: "square.and.arrow.down")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(model.isWorking)
            if model.isWorking {
                VStack(spacing: 6) {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(model.status).font(.caption).foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    if let progress = model.bundleProgress {
                        ProgressView(value: progress)
                            .progressViewStyle(.linear)
                            .frame(maxWidth: 320)
                        Text(model.bundleProgressLabel)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.regularMaterial)
    }
}

struct AppRow: View {
    let app: GuestApp
    let image: NSImage?

    var body: some View {
        HStack(spacing: 12) {
            AppIcon(image: image, size: 34)

            VStack(alignment: .leading, spacing: 2) {
                Text(app.name)
                    .font(.body.weight(.medium))
                    .lineLimit(1)
                Text(app.id)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            Spacer(minLength: 8)

            Image(systemName: app.installed ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(app.installed ? Color.green : Color.secondary)
                .font(.system(size: 14, weight: .semibold))
        }
        .padding(.vertical, 5)
    }
}

struct AppDetail: View {
    let app: GuestApp
    let image: NSImage?
    @ObservedObject var model: LibraryModel

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack(alignment: .center, spacing: 18) {
                AppIcon(image: image, size: 72)

                VStack(alignment: .leading, spacing: 5) {
                    Text(app.name)
                        .font(.largeTitle.weight(.semibold))
                        .lineLimit(1)
                    Text(app.id)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }

                Spacer()

                runButtons(for: app)
            }

            Divider()

            HStack(spacing: 14) {
                Label("Launchpad", systemImage: "square.grid.2x2.fill")
                    .font(.title3.weight(.semibold))
                Spacer()
                Toggle("", isOn: Binding(
                    get: { app.installed },
                    set: { enabled in model.setLaunchpad(app, enabled: enabled) }
                ))
                .toggleStyle(.switch)
                .labelsHidden()
                .disabled(model.isWorking)
            }

            VStack(alignment: .leading, spacing: 8) {
                Text(L("Command"))
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Text(app.execLine)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(2)
            }

            if !app.categories.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L("Category"))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Text(app.categories)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }

            Spacer()

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    if model.isWorking {
                        ProgressView()
                            .controlSize(.small)
                    }
                    Text(model.status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer()
                    if !model.bundleProgressLabel.isEmpty {
                        Text(model.bundleProgressLabel)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.tertiary)
                    }
                }
                if let progress = model.bundleProgress {
                    ProgressView(value: progress)
                        .progressViewStyle(.linear)
                }
            }
        }
        .padding(28)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.regularMaterial)
    }

    // Two run modes for the app. The bundle button is disabled with a hint
    // when the bundle is not installed.
    @ViewBuilder
    private func runButtons(for app: GuestApp) -> some View {
        HStack(spacing: 8) {
            Button {
                model.runAsBundle(app)
            } label: {
                Label(L("Bundle"), systemImage: "app.badge")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
            .disabled(!app.installed || model.isWorking)
            .help(app.installed
                  ? L("Bundle help installed")
                  : L("Bundle help missing"))

            Button {
                model.runInViewer(app)
            } label: {
                Label(L("Run in viewer"), systemImage: "rectangle.stack.badge.plus")
            }
            .controlSize(.large)
            .buttonStyle(.bordered)
            .disabled(model.isWorking)
            .help(L("Viewer help"))
        }
    }
}

struct RunningProcessesSheet: View {
    @ObservedObject var model: LibraryModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .background(.regularMaterial)
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "bolt.horizontal.circle.fill")
                .font(.title2)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(L("Running processes"))
                    .font(.headline)
                Text(L("Processes subtitle", arg: model.config.socket))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                Task { await model.refreshProcesses() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .disabled(model.isRefreshingProcesses)
            .help(L("Refresh processes"))

            Button(L("Close")) { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    @ViewBuilder
    private var content: some View {
        if model.runningProcesses.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "moon.zzz")
                    .font(.system(size: 32, weight: .semibold))
                    .foregroundStyle(.secondary)
                Text(L("No running apps"))
                    .font(.title3.weight(.semibold))
                Text(L("No running apps subtitle"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(28)
        } else {
            List(selection: $model.selectedRunningPID) {
                ForEach(model.runningProcesses) { proc in
                    ProcessRow(
                        proc: proc,
                        disabled: model.isRefreshingProcesses,
                        onKill: { model.killProcess(proc) }
                    )
                    .tag(Optional(proc.pid))
                }
            }
            .listStyle(.inset)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if model.isRefreshingProcesses {
                ProgressView().controlSize(.small)
            }
            Text(model.processesStatus)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer()
            Text(L("Auto refresh hint"))
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
    }
}

struct ProcessRow: View {
    let proc: RunningProcess
    let disabled: Bool
    let onKill: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "app.fill")
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 28, height: 28)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(proc.name)
                        .font(.body.weight(.semibold))
                        .lineLimit(1)
                    Text("PID \(proc.pid)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(.quaternary, in: Capsule())
                }
                Text(proc.command)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            Spacer(minLength: 8)

            VStack(alignment: .trailing, spacing: 4) {
                Text(uptimeText(from: proc.startedAt))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Button(role: .destructive) {
                    onKill()
                } label: {
                    Label(L("End"), systemImage: "stop.fill")
                }
                .controlSize(.small)
                .buttonStyle(.bordered)
                .tint(.red)
                .disabled(disabled)
            }
        }
        .padding(.vertical, 4)
    }

    private func uptimeText(from start: Date) -> String {
        let seconds = Int(Date().timeIntervalSince(start))
        if seconds < 60 { return L("started seconds ago {0}", arg: "\(seconds)") }
        if seconds < 3600 { return L("started minutes ago {0}", arg: "\(seconds / 60)") }
        let h = seconds / 3600
        let m = (seconds % 3600) / 60
        return m == 0 ? L("started hours ago {0}", arg: "\(h)") : L("started hours minutes ago {0} {1}", "\(h)", "\(m)")
    }
}

struct AppIcon: View {
    let image: NSImage?
    let size: CGFloat

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
            } else {
                ZStack {
                    RoundedRectangle(cornerRadius: size * 0.22, style: .continuous)
                        .fill(.thinMaterial)
                    Image(systemName: "app.dashed")
                        .font(.system(size: size * 0.45, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: size, height: size)
    }
}

// SettingsPopover has moved to Settings_SettingsPopover.swift.
// LanguagePickerRow lives in Settings_LanguagePickerRow.swift.

struct WindowConfigurator: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            window.titlebarAppearsTransparent = true
            window.toolbarStyle = .unified
            window.isMovableByWindowBackground = true
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - VBoxRunner — adapter for external ./vbox calls (single responsibility)
// Lifted out of LibraryModel's inline Process execution. When an
// activeGuestOverride is set, injects that machine's ssh user/host/dir as
// environment variables; otherwise uses the AppConfig defaults.
// ───────────────────────────────────────────────────────────────────────────

enum VBoxRunner {
    // Environment setup. Both run() and runStreaming() share the same env.
    private static func makeEnvironment(config: AppConfig, override: GuestMachine?) -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        env["VBOX_STATE_DIR"] = config.stateDir
        env["VBOX_GUEST"] = override?.sshString ?? config.guest
        env["VBOX_GUEST_DIR"] = override?.guestDir.isEmpty == false ? override!.guestDir : config.guestDir
        env["VBOX_PORT"] = config.port
        env["VBOX_SOCKET"] = config.socket
        env["VBOX_WIDTH"] = config.width
        env["VBOX_HEIGHT"] = config.height
        let hostChromeOn = UserDefaults.standard.object(forKey: VBoxDefaults.hostChromeKey) as? Bool ?? true
        env["VBOX_HOST_CHROME"] = hostChromeOn ? "1" : "0"
        return env
    }

    static func run(_ args: [String], config: AppConfig, override: GuestMachine?,
                    stdinText: String? = nil) async -> CommandResult {
        let process = makeProcess(args: args, config: config, override: override)
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        let stdinPipe: Pipe? = stdinText != nil ? Pipe() : nil
        process.standardInput = stdinPipe

        do {
            try process.run()
            if let stdinPipe, let data = stdinText?.data(using: .utf8) {
                stdinPipe.fileHandleForWriting.write(data)
                try? stdinPipe.fileHandleForWriting.close()
            }
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8) ?? ""
            return CommandResult(status: process.terminationStatus, output: output)
        } catch {
            return CommandResult(status: 1, output: error.localizedDescription)
        }
    }

    private static func makeProcess(args: [String], config: AppConfig,
                                    override: GuestMachine?) -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: config.cliPath)
        process.arguments = ["--no-build"] + args
        process.environment = makeEnvironment(config: config, override: override)
        return process
    }

    // Reads stdout/stderr line by line and invokes the onLine callback.
    // Intended for long-running commands that need progress reporting,
    // e.g. install-apps. onLine may be invoked from any thread, so the
    // caller must hop to @MainActor if needed.
    static func runStreaming(_ args: [String],
                             config: AppConfig,
                             override: GuestMachine?,
                             onLine: @escaping @Sendable (String) -> Void) async -> CommandResult {
        let process = makeProcess(args: args, config: config, override: override)
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe

        let buffer = VBoxRunnerLineBuffer()
        let handle = pipe.fileHandleForReading
        handle.readabilityHandler = { fh in
            let data = fh.availableData
            guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
            for line in buffer.append(text) {
                onLine(line)
            }
        }

        do {
            try process.run()
            process.waitUntilExit()
            handle.readabilityHandler = nil
            // Flush whatever is left in the buffer.
            for line in buffer.flushRemaining() { onLine(line) }
            return CommandResult(status: process.terminationStatus, output: buffer.fullOutput())
        } catch {
            handle.readabilityHandler = nil
            return CommandResult(status: 1, output: error.localizedDescription)
        }
    }
}

// Line-oriented buffer (Sendable, safe to access from any thread).
final class VBoxRunnerLineBuffer: @unchecked Sendable {
    private var partial = ""
    private var all = ""
    private let lock = NSLock()

    func append(_ chunk: String) -> [String] {
        lock.lock(); defer { lock.unlock() }
        all += chunk
        partial += chunk
        var lines: [String] = []
        while let r = partial.range(of: "\n") {
            lines.append(String(partial[..<r.lowerBound]))
            partial.removeSubrange(partial.startIndex..<r.upperBound)
        }
        return lines
    }

    func flushRemaining() -> [String] {
        lock.lock(); defer { lock.unlock() }
        guard !partial.isEmpty else { return [] }
        let last = partial
        partial = ""
        return [last]
    }

    func fullOutput() -> String {
        lock.lock(); defer { lock.unlock() }
        return all
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - GuestMachine data model (pure value type)
// ───────────────────────────────────────────────────────────────────────────

enum MachineStatus: String {
    case running
    case stopped
    case suspended
    case paused
    case invalid
    case remote    // User-added remote SSH host — a machine outside prlctl.
    case unknown

    init(rawText: String) {
        self = MachineStatus(rawValue: rawText.lowercased()) ?? .unknown
    }

    // Used to decide if the machine can be picked in the GUI. `remote` is
    // always selectable because it is reachable via ssh.
    var isRunning: Bool { self == .running || self == .remote }
    var isLaunchable: Bool {
        // States vbox can boot via prlctl. `remote` is external — we can't
        // control it, so the boot button is hidden.
        switch self {
        case .running, .stopped, .suspended, .paused: return true
        case .invalid, .remote, .unknown: return false
        }
    }

    var displayLabel: String {
        switch self {
        case .running:   return L("Status running")
        case .stopped:   return L("Status stopped")
        case .suspended: return L("Status suspended")
        case .paused:    return L("Status paused")
        case .invalid:   return L("Status invalid")
        case .remote:    return L("Status remote")
        case .unknown:   return L("Status unknown")
        }
    }
}

enum MachineOSKind: String {
    case linux, windows, macos, unknown

    init(rawText: String) {
        self = MachineOSKind(rawValue: rawText.lowercased()) ?? .unknown
    }

    var isSupportedByVBox: Bool { self == .linux }

    // SF Symbol name. For Linux we prefer the PNG cache; this is the
    // fallback ("terminal") when none is available, so any future caller
    // still gets a valid SF Symbol.
    var iconSystemName: String {
        switch self {
        case .linux:   return "terminal"
        case .windows: return "macwindow"
        case .macos:   return "applelogo"
        case .unknown: return "questionmark.diamond"
        }
    }
}

// Returns a PNG path inside the cache directory by inspecting the raw distro
// string.
// Priority: distro-specific PNG → tux.png. nil when neither exists.
// The PNG files are populated by `./vbox distro-icons fetch`, which pulls
// them from devicon.
func linuxDistroIconPath(_ osRaw: String, name: String = "", iconDir: String) -> String? {
    guard !iconDir.isEmpty else { return nil }
    let s = (osRaw + " " + name).lowercased()
    var ids: [String] = []
    if s.contains("fedora") { ids.append("fedora") }
    if s.contains("ubuntu") { ids.append("ubuntu") }
    if s.contains("debian") { ids.append("debian") }
    if s.contains("arch")   { ids.append("arch") }
    if s.contains("centos") || s.contains("rhel") || s.contains("rocky") || s.contains("alma") {
        ids.append("centos")
    }
    if s.contains("mint") { ids.append("mint") }
    if s.contains("suse") || s.contains("opensuse") { ids.append("opensuse") }
    // Uncached distros (alpine, kali, etc.) automatically fall back to Tux.
    ids.append("tux")
    for id in ids {
        let path = "\(iconDir)/\(id).png"
        if FileManager.default.fileExists(atPath: path) {
            return path
        }
    }
    return nil
}

struct GuestMachine: Identifiable, Equatable, Hashable {
    let uuid: String
    let name: String
    let status: MachineStatus
    let ip: String
    let osRaw: String
    let osKind: MachineOSKind
    let sshUser: String
    let sshHost: String
    let guestDir: String

    var id: String { uuid }

    // "user@host" form. Returns empty string when host is empty (caller
    // disables the UI in that case).
    var sshString: String {
        guard !sshHost.isEmpty else { return "" }
        return "\(sshUser)@\(sshHost)"
    }

    // Can the GUI pick this machine as the active guest? Requires Linux +
    // (running or remote) + a non-empty IP.
    var isSelectable: Bool {
        osKind.isSupportedByVBox && status.isRunning && !sshHost.isEmpty
    }

    // A user-added remote host (outside Parallels) — used to branch on
    // things like exposing the delete button.
    var isRemote: Bool { uuid.hasPrefix("remote:") }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachineListParser — vbox machines --tsv → [GuestMachine]
// Pure function: output is determined entirely by input, so it is easy to
// test.
// ───────────────────────────────────────────────────────────────────────────

enum MachineListParser {
    static func parse(_ tsv: String) -> [GuestMachine] {
        tsv.split(separator: "\n", omittingEmptySubsequences: true).compactMap { line in
            let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
            guard parts.count >= 9 else { return nil }
            return GuestMachine(
                uuid: parts[0],
                name: parts[1],
                status: MachineStatus(rawText: parts[2]),
                ip: parts[3],
                osRaw: parts[4],
                osKind: MachineOSKind(rawText: parts[5]),
                sshUser: parts[6],
                sshHost: parts[7],
                guestDir: parts[8]
            )
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachinesModel — machine-list state + actions
// Split out of LibraryModel. Owns only the vbox-machines calls. When the
// selection changes, the onSelect callback notifies the outside world
// (LibraryModel).
// ───────────────────────────────────────────────────────────────────────────

@MainActor
final class MachinesModel: ObservableObject {
    @Published var machines: [GuestMachine] = []
    @Published var selectedUUID: String? = nil
    @Published var isRefreshing = false
    @Published var status = ""

    let config: AppConfig
    var onSelect: ((GuestMachine?) -> Void)?

    init(config: AppConfig) {
        self.config = config
    }

    var selectedMachine: GuestMachine? {
        guard let uuid = selectedUUID else { return nil }
        return machines.first(where: { $0.uuid == uuid })
    }

    func refresh() async {
        isRefreshing = true
        status = L("Refresh")
        let result = await VBoxRunner.run(["machines", "list", "--tsv"], config: config, override: nil)
        if result.status == 0 {
            let parsed = MachineListParser.parse(result.output)
            machines = parsed
            if let selectedUUID, !parsed.contains(where: { $0.uuid == selectedUUID }) {
                self.selectedUUID = nil
                onSelect?(nil)
            }
            status = parsed.isEmpty ? L("No machines") : "\(parsed.count)대 발견"
        } else {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Refresh failed")
        }
        isRefreshing = false
    }

    func select(_ machine: GuestMachine?) {
        guard let machine else {
            selectedUUID = nil
            onSelect?(nil)
            status = ""
            return
        }
        // For non-selectable machines, surface the reason in the footer
        // instead of silently returning.
        guard machine.isSelectable else {
            status = unselectableReason(for: machine)
            return
        }
        selectedUUID = machine.uuid
        onSelect?(machine)
        status = L("Active machine no count {0}", arg: machine.name)
    }

    private func unselectableReason(for m: GuestMachine) -> String {
        m.name + ": " + L("Cannot select")
    }

    func start(_ machine: GuestMachine) async {
        await runControl(args: ["machines", "start", machine.uuid],
                        busyLabel: L("Power on VM") + " — " + machine.name)
    }

    func stop(_ machine: GuestMachine) async {
        await runControl(args: ["machines", "stop", machine.uuid],
                        busyLabel: L("Power off VM") + " — " + machine.name)
    }

    // Loads per-machine config (overrides) via
    // `./vbox machines config UUID --json`.
    func loadConfig(for machine: GuestMachine) async -> [String: String] {
        let result = await VBoxRunner.run(
            ["machines", "config", machine.uuid, "--json"],
            config: config, override: nil)
        guard result.status == 0,
              let data = result.output.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return [:] }
        var out: [String: String] = [:]
        for (k, v) in obj { out[k] = String(describing: v) }
        return out
    }

    // Calls set/unset only for changed keys. Empty value means unset
    // (falls back to the default).
    func saveConfig(for machine: GuestMachine, changes: [(String, String)]) async -> Bool {
        var ok = true
        for (key, value) in changes {
            let args: [String]
            if value.isEmpty {
                args = ["machines", "unset", machine.uuid, key]
            } else {
                args = ["machines", "set", machine.uuid, key, value]
            }
            let result = await VBoxRunner.run(args, config: config, override: nil)
            if result.status != 0 { ok = false }
        }
        status = ok ? L("Saved settings {0}", arg: machine.name) : L("Save failed")
        await refresh()
        return ok
    }

    // Registers a remote host — calls `vbox remote add` and refreshes the
    // list afterwards.
    func setPassword(for machine: GuestMachine, password: String) async -> Bool {
        let result = await VBoxRunner.run(
            ["machines", "set", machine.uuid, "password", password],
            config: config, override: nil)
        return result.status == 0
    }

    func clearPassword(for machine: GuestMachine) async -> Bool {
        let result = await VBoxRunner.run(
            ["machines", "unset", machine.uuid, "password"],
            config: config, override: nil)
        return result.status == 0
    }

    func addRemote(name: String, ssh: String, guestDir: String, osRaw: String,
                   identityFile: String = "", password: String = "") async -> Bool {
        var args = ["remote", "add", "--name", name, "--ssh", ssh]
        if !guestDir.isEmpty     { args += ["--dir", guestDir] }
        if !osRaw.isEmpty        { args += ["--os-raw", osRaw] }
        if !identityFile.isEmpty { args += ["--identity-file", identityFile] }
        let stdinText: String?
        if !password.isEmpty {
            args.append("--password-stdin")
            stdinText = password
        } else {
            stdinText = nil
        }
        let result = await VBoxRunner.run(args, config: config, override: nil, stdinText: stdinText)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Add remote failed")
            return false
        }
        status = L("Add remote ok {0}", arg: name)
        await refresh()
        return true
    }

    // Removes a remote host — if the deleted machine was the selected one,
    // also clears the selection and the override.
    func removeRemote(_ machine: GuestMachine) async {
        guard machine.isRemote else { return }
        let result = await VBoxRunner.run(["remote", "remove", machine.name],
                                          config: config, override: nil)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Remove remote failed")
            return
        }
        if selectedUUID == machine.uuid {
            selectedUUID = nil
            onSelect?(nil)
        }
        status = L("Remove remote ok {0}", arg: machine.name)
        await refresh()
    }

    private func runControl(args: [String], busyLabel: String) async {
        isRefreshing = true
        status = busyLabel
        let result = await VBoxRunner.run(args, config: config, override: nil)
        if result.status != 0 {
            status = result.output
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n").first ?? L("Run failed")
            isRefreshing = false
            return
        }
        // Small delay so prlctl can settle the state after start/stop.
        try? await Task.sleep(nanoseconds: 1_500_000_000)
        await refresh()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// MARK: - MachineRow / MachinesSheet — machine-list UI
// Each view sticks to its own responsibility: row renders a single line
// plus action callbacks; sheet renders the list plus the footer.
// ───────────────────────────────────────────────────────────────────────────

struct MachineStatusBadge: View {
    let status: MachineStatus

    var body: some View {
        Text(status.displayLabel)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch status {
        case .running:   return .green
        case .stopped:   return .secondary
        case .suspended, .paused: return .orange
        case .invalid:   return .red
        case .remote:    return .blue
        case .unknown:   return .secondary
        }
    }
}

struct MachineRow: View {
    let machine: GuestMachine
    let isActive: Bool
    let isBusy: Bool
    let distroIconDir: String
    let onSelect: () -> Void
    let onStart: () -> Void
    let onStop: () -> Void
    let onRemove: () -> Void  // remote machines only
    let onConfig: () -> Void  // opens the machine-settings sheet
    let onInfo: () -> Void    // opens the machine-info sheet

    var body: some View {
        HStack(spacing: 12) {
            // Only the left-side info is the tap target. Buttons in `controls`
            // keep their own hit-test.
            HStack(spacing: 12) {
                osIcon
                    .frame(width: 28, height: 28)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(machine.name)
                            .font(.body.weight(.semibold))
                            .lineLimit(1)
                            .foregroundStyle(machine.osKind.isSupportedByVBox ? .primary : .secondary)
                        MachineStatusBadge(status: machine.status)
                        if isActive {
                            Text(L("Active"))
                                .font(.caption2.weight(.bold))
                                .padding(.horizontal, 6).padding(.vertical, 1)
                                .background(Color.accentColor.opacity(0.2), in: Capsule())
                                .foregroundStyle(Color.accentColor)
                        }
                    }
                    Text(detailLine)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
            }
            .contentShape(Rectangle())
            .onTapGesture { onSelect() }
            .help(machine.isSelectable ? L("Tap to activate guest") : L("Cannot activate"))

            controls
        }
        .padding(.vertical, 4)
        .opacity(machine.isSelectable ? 1.0 : 0.55)
    }

    private var detailLine: String {
        let ip = machine.ip.isEmpty || machine.ip == "-" ? "ip ?" : machine.ip
        let os = machine.osRaw.isEmpty ? machine.osKind.rawValue : machine.osRaw
        return "\(os) · \(ip)"
    }

    @ViewBuilder
    private var osIcon: some View {
        if machine.osKind == .linux,
           let path = linuxDistroIconPath(machine.osRaw, name: machine.name, iconDir: distroIconDir),
           let img = NSImage(contentsOfFile: path) {
            Image(nsImage: img)
                .resizable()
                .aspectRatio(contentMode: .fit)
        } else {
            // Falls back to an SF Symbol when the distro PNG cache is
            // missing or the OS is not Linux.
            Image(systemName: machine.osKind.iconSystemName)
                .font(.title3)
                .foregroundStyle(machine.osKind.isSupportedByVBox ? Color.accentColor : .secondary)
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 6) {
            // Parallels VM: start/stop based on state. For remote hosts
            // vbox cannot control boot, so we show a delete button instead.
            if machine.isRemote {
                Button(role: .destructive) { onRemove() } label: { Image(systemName: "trash") }
                    .help(L("Remove remote host"))
                    .controlSize(.small)
                    .buttonStyle(.bordered)
                    .disabled(isBusy)
            } else if machine.status == .running {
                Button { onStop() } label: { Image(systemName: "stop.fill") }
                    .help(L("Power off VM"))
                    .controlSize(.small).buttonStyle(.bordered).disabled(isBusy)
            } else if machine.status.isLaunchable {
                Button { onStart() } label: { Image(systemName: "play.fill") }
                    .help(L("Power on VM"))
                    .controlSize(.small).buttonStyle(.bordered).disabled(isBusy)
            }
            Button { onInfo() } label: { Image(systemName: "info.circle") }
                .help(L("Machine info"))
                .controlSize(.small)
                .buttonStyle(.bordered)
                .disabled(isBusy)
            Button { onConfig() } label: { Image(systemName: "gearshape") }
                .help(L("Open settings"))
                .controlSize(.small)
                .buttonStyle(.bordered)
                .disabled(isBusy)
            Button { onSelect() } label: {
                Image(systemName: isActive ? "checkmark.circle.fill" : "arrow.right.circle")
            }
            .help(machine.isSelectable ? L("Activate guest") : L("Cannot select"))
            .controlSize(.small)
            .buttonStyle(.bordered)
            .disabled(!machine.isSelectable || isBusy)
        }
    }
}

struct MachinesSheet: View {
    @ObservedObject var model: MachinesModel
    let activeUUID: String?
    let onConfigRequest: (GuestMachine) -> Void   // nested-sheet workaround: parent presents
    let onAddRemoteRequest: () -> Void            // same reason
    let onInfoRequest: (GuestMachine) -> Void     // same reason — read-only info
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .background(.regularMaterial)
        .frame(minWidth: 620, minHeight: 420)
        .task {
            if model.machines.isEmpty {
                await model.refresh()
            }
        }
        // macOS SwiftUI does not present nested sheets (a sheet inside the
        // machines sheet). So both follow-up sheets are presented by
        // LibraryWindow via the onConfigRequest / onAddRemoteRequest
        // callbacks.
    }

    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "rectangle.3.group.fill")
                .font(.title2).foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 2) {
                Text(L("Guest Machines"))
                    .font(.headline)
                Text(L("Machines subtitle"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button {
                onAddRemoteRequest()
            } label: { Image(systemName: "plus") }
                .help(L("Add remote host"))
                .disabled(model.isRefreshing)
            Button {
                Task { await model.refresh() }
            } label: { Image(systemName: "arrow.clockwise") }
                .disabled(model.isRefreshing)
                .help(L("Refresh"))
            Button(L("Close")) { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    @ViewBuilder
    private var content: some View {
        if model.machines.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "tray")
                    .font(.system(size: 32, weight: .semibold))
                    .foregroundStyle(.secondary)
                Text(L("No machines")).font(.title3.weight(.semibold))
                Text(L("No machines hint"))
                    .font(.caption).foregroundStyle(.secondary)
                Button { onAddRemoteRequest() } label: {
                    Label(L("Add remote host"), systemImage: "plus")
                }
                .buttonStyle(.borderedProminent)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(28)
        } else {
            List {
                ForEach(model.machines) { machine in
                    MachineRow(
                        machine: machine,
                        isActive: machine.uuid == activeUUID,
                        isBusy: model.isRefreshing,
                        distroIconDir: model.config.distroIconDir,
                        onSelect: { model.select(machine) },
                        onStart: { Task { await model.start(machine) } },
                        onStop:  { Task { await model.stop(machine) } },
                        onRemove: { Task { await model.removeRemote(machine) } },
                        onConfig: { onConfigRequest(machine) },
                        onInfo: { onInfoRequest(machine) }
                    )
                }
            }
            .listStyle(.inset)
        }
    }

    private var footer: some View {
        HStack(spacing: 10) {
            if model.isRefreshing {
                ProgressView().controlSize(.small)
            }
            Text(model.status).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            Spacer()
        }
        .padding(.horizontal, 20).padding(.vertical, 10)
    }
}

// Input form for a new remote SSH host. Calls onSubmit after validation.
// Form for adding a remote SSH host. Shares the same visual language as
// MachineConfigSheet (grouped Form + labeledField helper). Required: name
// + SSH target. Everything else is optional.
struct AddRemoteSheet: View {
    let onSubmit: (_ name: String, _ ssh: String, _ guestDir: String, _ osRaw: String,
                   _ identityFile: String, _ password: String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var ssh = ""
    @State private var guestDir = ""
    @State private var osRaw = ""
    @State private var identityFile = ""
    @State private var password = ""

    private var canSubmit: Bool {
        !name.trimmed().isEmpty
            && ssh.contains("@")
            && !ssh.hasPrefix("@") && !ssh.hasSuffix("@")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "network").font(.title2).foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("Add remote host")).font(.headline)
                    Text(L("Add remote subtitle"))
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    configSectionBox(L("Required info")) {
                        labeledField(L("Display name"), placeholder: L("Display name placeholder"),
                                     text: $name,
                                     hint: L("Display name hint"))
                        labeledField(L("SSH target"), placeholder: L("SSH target placeholder"),
                                     text: $ssh,
                                     hint: L("SSH target hint"))
                    }

                    configSectionBox(L("Optional fields")) {
                        identityFileRow(text: $identityFile,
                                        hint: L("Field identity hint"))
                        passwordRow(text: $password,
                                    placeholder: L("Field password placeholder"),
                                    hint: L("Field password hint"))
                        labeledField(L("Workdir"), placeholder: L("Workdir placeholder"),
                                     text: $guestDir,
                                     hint: L("Workdir hint placeholder"))
                        labeledField(L("OS label"), placeholder: L("OS label placeholder"),
                                     text: $osRaw,
                                     hint: L("OS label hint"))
                    }
                }
                .padding(.vertical, 4)
            }

            HStack {
                Spacer()
                Button(L("Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(L("Add")) {
                    onSubmit(name.trimmed(), ssh.trimmed(),
                             guestDir.trimmed(), osRaw.trimmed(),
                             identityFile.trimmed(), password)
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(!canSubmit)
            }
        }
        .padding(20)
        .frame(minWidth: 560, idealWidth: 620, minHeight: 620, idealHeight: 660)
    }
}

// Form for editing per-machine overrides. Loads current values via
// `vbox machines config`, then on save issues set/unset only for changed
// keys. Empty value = unset (falls back to the default).
struct MachineConfigSheet: View {
    let machine: GuestMachine
    @ObservedObject var model: MachinesModel
    @Environment(\.dismiss) private var dismiss

    @State private var sshUser = ""
    @State private var sshHost = ""
    @State private var sshPort = ""
    @State private var identityFile = ""
    @State private var password = ""
    @State private var clearPasswordRequested = false
    @State private var originalHasPassword = false
    @State private var guestDir = ""
    @State private var port = ""
    @State private var socket = ""
    @State private var width = ""
    @State private var height = ""
    @State private var debug = false
    @State private var tls = false
    @State private var notes = ""
    @State private var original: [String: String] = [:]
    @State private var isLoading = false
    @State private var isSaving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                Image(systemName: "gearshape.fill").font(.title2).foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L("Open settings")).font(.headline)
                    Text(machine.name).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
            }

            if isLoading {
                ProgressView(L("Loading")).frame(maxWidth: .infinity)
            } else {
                ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        Color.clear.frame(height: 1).id("top")
                        configSectionBox(L("SSH section"),
                                         footer: L("SSH footer")) {
                            labeledField(L("Field user"), placeholder: L("Field user placeholder"),
                                         currentValue: machine.sshUser,
                                         text: $sshUser,
                                         hint: L("Field user hint"))
                            labeledField(L("Field host"), placeholder: L("Field host placeholder"),
                                         currentValue: machine.sshHost,
                                         text: $sshHost,
                                         hint: L("Field host hint"))
                            labeledField(L("Field ssh port"), placeholder: L("Field ssh port placeholder"),
                                         text: $sshPort,
                                         hint: L("Field ssh port hint"))
                            identityFileRow(text: $identityFile,
                                            hint: L("Field identity hint"))
                            passwordChangeRow(text: $password,
                                              hasExisting: originalHasPassword,
                                              clearRequested: $clearPasswordRequested,
                                              hint: L("Field password hint"))
                        }

                        configSectionBox(L("Guest path section")) {
                            labeledField(L("Field guestdir"), placeholder: L("Field guestdir placeholder"),
                                         currentValue: machine.guestDir,
                                         text: $guestDir,
                                         hint: L("Field guestdir hint"))
                        }

                        configSectionBox(L("Viewer section"),
                                         footer: L("Viewer footer")) {
                            labeledField(L("Field port"), placeholder: L("Field port placeholder"),
                                         text: $port,
                                         hint: L("Field port hint"))
                            labeledField(L("Field socket"), placeholder: L("Field socket placeholder"),
                                         text: $socket,
                                         hint: L("Field socket hint"))
                            labeledField(L("Field width"), placeholder: L("Field width placeholder"),
                                         text: $width,
                                         hint: L("Field width hint"))
                            labeledField(L("Field height"), placeholder: L("Field height placeholder"),
                                         text: $height,
                                         hint: L("Field height hint"))
                        }

                        configSectionBox(L("Flags section")) {
                            flagToggle(title: L("Flag debug"),
                                       hint: L("Flag debug hint"),
                                       isOn: $debug)
                            flagToggle(title: L("Flag tls"),
                                       hint: L("Flag tls hint"),
                                       isOn: $tls)
                        }

                        configSectionBox(L("Memo section"),
                                         footer: L("Memo footer")) {
                            TextField(L("Memo placeholder"),
                                      text: $notes, axis: .vertical)
                                .textFieldStyle(.roundedBorder)
                                .multilineTextAlignment(.leading)
                                .lineLimit(3...6)
                        }
                    }
                    .padding(.vertical, 4)
                }
                .onAppear { proxy.scrollTo("top", anchor: .top) }
                }
            }

            HStack {
                Spacer()
                Button(L("Cancel")) { dismiss() }.keyboardShortcut(.cancelAction)
                Button(isSaving ? L("Saving") : L("Save")) {
                    Task { await save() }
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(isLoading || isSaving)
            }
        }
        .padding(20)
        .frame(minWidth: 560, idealWidth: 600, minHeight: 720, idealHeight: 780)
        .task { await load() }
    }

    private func load() async {
        isLoading = true
        let map = await model.loadConfig(for: machine)
        original = map
        sshUser = map["ssh_user"] ?? ""
        sshHost = map["ssh_host"] ?? ""
        sshPort = map["ssh_port"] ?? ""
        identityFile = map["identity_file"] ?? ""
        password = ""
        clearPasswordRequested = false
        originalHasPassword = (map["has_password"] ?? "") == "true"
        guestDir = map["guest_dir"] ?? ""
        port = map["port"] ?? ""
        socket = map["socket"] ?? ""
        width = map["width"] ?? ""
        height = map["height"] ?? ""
        debug = (map["debug"] ?? "") == "true"
        tls = (map["tls"] ?? "") == "true"
        notes = map["notes"] ?? ""
        isLoading = false
    }

    private func save() async {
        isSaving = true
        let current: [(String, String)] = [
            ("ssh_user", sshUser.machineConfigTrim()),
            ("ssh_host", sshHost.machineConfigTrim()),
            ("ssh_port", sshPort.machineConfigTrim()),
            ("identity_file", identityFile.machineConfigTrim()),
            ("guest_dir", guestDir.machineConfigTrim()),
            ("port", port.machineConfigTrim()),
            ("socket", socket.machineConfigTrim()),
            ("width", width.machineConfigTrim()),
            ("height", height.machineConfigTrim()),
            ("debug", debug ? "true" : ""),
            ("tls", tls ? "true" : ""),
            ("notes", notes),
        ]
        let changes = current.filter { (k, v) in (original[k] ?? "") != v }
        if !changes.isEmpty {
            _ = await model.saveConfig(for: machine, changes: changes)
        }
        if !password.isEmpty {
            _ = await model.setPassword(for: machine, password: password)
        } else if clearPasswordRequested && originalHasPassword {
            _ = await model.clearPassword(for: machine)
        }
        isSaving = false
        dismiss()
    }
}

private extension String {
    func machineConfigTrim() -> String { trimmingCharacters(in: .whitespacesAndNewlines) }
    func trimmed() -> String { trimmingCharacters(in: .whitespacesAndNewlines) }
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

// Left-aligned settings-section box. Hand-rolled with ScrollView + VStack +
// custom GroupBox styling to avoid macOS Form's automatic LabeledContent
// conversion.
@ViewBuilder
private func configSectionBox<Content: View>(_ title: String,
                                             footer: String = "",
                                             @ViewBuilder content: () -> Content) -> some View {
    VStack(alignment: .leading, spacing: 8) {
        Text(title)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
        VStack(alignment: .leading, spacing: 14) {
            content()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.background.opacity(0.6), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .strokeBorder(.quaternary, lineWidth: 1)
        )
        if !footer.isEmpty {
            Text(footer)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// Flag toggle: label + hint on the left, toggle on the right. SwiftUI
// Toggle's standard label-trailing pattern.
@ViewBuilder
private func flagToggle(title: String, hint: String, isOn: Binding<Bool>) -> some View {
    HStack(alignment: .top) {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.callout.weight(.medium))
            Text(hint).font(.caption2).foregroundStyle(.secondary)
        }
        Spacer()
        Toggle("", isOn: isOn).labelsHidden()
    }
}

// One-line settings field: label → input → hint + "Current: <default>"
// auxiliary info.
// Input text / placeholder / cursor are all leading-aligned. currentValue is
// rendered on its own line, separate from hint, for visual clarity.
@ViewBuilder
private func labeledField(_ label: String,
                          placeholder: String,
                          currentValue: String = "",
                          text: Binding<String>,
                          hint: String) -> some View {
    VStack(alignment: .leading, spacing: 4) {
        Text(label)
            .font(.callout.weight(.medium))
            .frame(maxWidth: .infinity, alignment: .leading)
        TextField(placeholder, text: text)
            .textFieldStyle(.roundedBorder)
            .multilineTextAlignment(.leading)
        Text(hint)
            .font(.caption2)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
        if !currentValue.isEmpty {
            HStack(spacing: 4) {
                Text(L("Current label"))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                Text(currentValue)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
}

@main
struct VBoxLibraryApp: App {
    // App-wide shared LocalizationStore. The global L() reads through a
    // nonisolated cache, so string lookup works even without this
    // injection — but injecting it as an EnvironmentObject is what lets
    // SwiftUI view bodies re-render immediately when `selected` changes.
    @StateObject private var localization = LocalizationStore.shared

    var body: some Scene {
        WindowGroup("vbox") {
            LibraryWindow()
                .environmentObject(localization)
        }
        .windowToolbarStyle(.unified)
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
    }
}
