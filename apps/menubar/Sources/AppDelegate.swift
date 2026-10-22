import AppKit
import Darwin
import Foundation

private struct CommandResult {
    let exitCode: Int32
    let output: String
}

private struct LocalStatus: Decodable {
    struct Runtime: Decodable {
        let runtime_id: String
    }

    let state: String
    let runtime: Runtime?
}

private struct LocalLoginPreferences: Decodable {
    let start_folder: String?
    let launch_tunnel_at_login: Bool
    let automatic_updates: Bool
    let liveboard_port: UInt16
}

private struct AutomaticUpdateClaim: Decodable {
    let due: Bool
}

private struct UpgradeEvent: Decodable {
    let schema_version: Int
    let event: String
    let current_version: String
    let target_version: String?
    let update_available: Bool?
    let direction: String?
    let local_state: String?
    let local_blocks_upgrade: Bool?
    let code: String?
    let message: String
}

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private static let startFolderKey = "startFolder"

    private let zodexURL: URL
    private let defaults = UserDefaults.standard
    private let menu = NSMenu()
    private var statusItem: NSStatusItem!

    private lazy var startItem = menuItem(
        "Start Zodex",
        action: #selector(startZodex),
        symbol: "play.fill",
        symbolDescription: "Start"
    )
    private lazy var stopItem = menuItem(
        "Stop Zodex",
        action: #selector(stopZodex),
        symbol: "stop.fill",
        symbolDescription: "Stop"
    )
    private lazy var liveboardItem = menuItem(
        "Open Liveboard",
        action: #selector(openLiveboard),
        symbol: "arrow.up.right",
        symbolDescription: "Liveboard"
    )
    private lazy var copyLiveboardItem = menuItem(
        "Copy Liveboard Link",
        action: #selector(copyLiveboardLink),
        symbol: "doc.on.doc",
        symbolDescription: "Copy Liveboard link"
    )
    private lazy var startFolderItem: NSMenuItem = {
        let item = NSMenuItem(title: "Start Folder: Not set", action: nil, keyEquivalent: "")
        item.image = menuSymbol("folder.fill", description: "Start folder")
        item.isEnabled = false
        return item
    }()
    private lazy var changeFolderItem = menuItem(
        "Change Start Folder…",
        action: #selector(changeStartFolder),
        symbol: "folder.fill",
        symbolDescription: "Start folder"
    )
    private lazy var launchAtLoginItem = menuItem(
        "Launch Menu Bar at Login",
        action: #selector(toggleLaunchAtLogin)
    )
    private lazy var launchTunnelAtLoginItem = menuItem(
        "Launch Tunnel at Login",
        action: #selector(toggleLaunchTunnelAtLogin)
    )
    private lazy var updateItem = menuItem(
        "Check for Updates…",
        action: #selector(updateZodex)
    )
    private lazy var automaticUpdatesItem = menuItem(
        "Automatic Updates (Daily)",
        action: #selector(toggleAutomaticUpdates)
    )
    private lazy var liveboardPortItem = menuItem(
        "Set Liveboard Port… (64973)",
        action: #selector(setLiveboardPort)
    )
    private lazy var versionItem: NSMenuItem = {
        let version = Bundle.main.object(forInfoDictionaryKey: "ZodexVersion") as? String
        let item = NSMenuItem(title: "Zodex v\(version ?? "unknown")", action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }()
    private lazy var quitItem = menuItem("Quit Menu Bar", action: #selector(quit))

    private var commandInFlight = false
    private var currentRuntimeID: String?
    private var availableUpdateVersion: String?
    private var upgradeCheckInFlight = false
    private var upgradeInFlight = false
    private var sharedStartFolder: String?
    private var tunnelAutostartEnabled = false
    private var automaticUpdatesEnabled = true
    private var liveboardPort: UInt16 = 64973
    private var pendingAutomaticUpdateVersion: String?
    private var automaticUpdateTimer: Timer?

    init(zodexURL: URL) {
        self.zodexURL = zodexURL
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem.button {
            button.image = nil
            button.title = "Z"
            button.font = NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
            button.toolTip = "Zodex"
        }

        menu.delegate = self
        menu.autoenablesItems = false
        menu.addItem(startItem)
        menu.addItem(stopItem)
        menu.addItem(liveboardItem)
        menu.addItem(copyLiveboardItem)
        menu.addItem(.separator())
        let configureMenu = NSMenu(title: "Configure")
        configureMenu.autoenablesItems = false
        configureMenu.addItem(startFolderItem)
        configureMenu.addItem(changeFolderItem)
        configureMenu.addItem(liveboardPortItem)
        configureMenu.addItem(.separator())
        configureMenu.addItem(updateItem)
        configureMenu.addItem(automaticUpdatesItem)
        configureMenu.addItem(.separator())
        configureMenu.addItem(launchAtLoginItem)
        configureMenu.addItem(launchTunnelAtLoginItem)
        configureMenu.addItem(versionItem)
        let configureItem = NSMenuItem(title: "Configure", action: nil, keyEquivalent: "")
        configureItem.submenu = configureMenu
        menu.addItem(configureItem)
        menu.addItem(.separator())
        menu.addItem(quitItem)
        statusItem.menu = menu

        refreshLoginPreferences(migrateLegacyFolder: true)
        updateStartFolderItems()
        updateLaunchAtLoginItem()
        disableRuntimeActions()
        if tunnelAutostartEnabled, let path = validStartFolder {
            // Login startup is opt-in. Launch asynchronously through the same
            // captured interactive shell used for a manual menu Start.
            beginStart(path: path, showFailure: false)
        }
        // One daily claim across process restarts, with a lightweight timer
        // while the menu app is open. Never replace an active Local runtime.
        automaticUpdateTimer = Timer.scheduledTimer(withTimeInterval: 15 * 60, repeats: true) {
            [weak self] _ in
            self?.checkAutomaticUpdatesIfDue()
            self?.applyPendingAutomaticUpdateIfIdle()
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 30) { [weak self] in
            self?.checkAutomaticUpdatesIfDue()
            self?.applyPendingAutomaticUpdateIfIdle()
        }
    }

    func menuWillOpen(_ menu: NSMenu) {
        copyLiveboardItem.title = "Copy Liveboard Link"
        refreshLoginPreferences()
        updateStartFolderItems()
        updateLaunchAtLoginItem()
        refreshStatus()
        applyPendingAutomaticUpdateIfIdle()
    }

    private func menuItem(
        _ title: String,
        action: Selector,
        symbol: String? = nil,
        symbolDescription: String? = nil
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        if let symbol {
            item.image = menuSymbol(symbol, description: symbolDescription ?? title)
        }
        return item
    }

    private func menuSymbol(_ name: String, description: String) -> NSImage? {
        let image = NSImage(systemSymbolName: name, accessibilityDescription: description)
        image?.isTemplate = true
        return image
    }

    private var configuredStartFolder: String {
        sharedStartFolder
            ?? defaults.string(forKey: Self.startFolderKey)
            ?? FileManager.default.homeDirectoryForCurrentUser.path
    }

    private func readLoginPreferences() -> LocalLoginPreferences? {
        let result = runZodexSync(["local", "autostart", "status", "--json"])
        guard result.exitCode == 0, let data = result.output.data(using: .utf8) else {
            return nil
        }
        return try? JSONDecoder().decode(LocalLoginPreferences.self, from: data)
    }

    private func refreshLoginPreferences(migrateLegacyFolder: Bool = false) {
        guard var preferences = readLoginPreferences() else {
            launchTunnelAtLoginItem.state = .off
            return
        }
        if migrateLegacyFolder,
           preferences.start_folder == nil,
           let legacyPath = defaults.string(forKey: Self.startFolderKey) {
            let result = runZodexSync(["local", "autostart", "folder", legacyPath])
            if result.exitCode == 0 {
                preferences = readLoginPreferences() ?? preferences
            }
        }
        sharedStartFolder = preferences.start_folder
        tunnelAutostartEnabled = preferences.launch_tunnel_at_login
        automaticUpdatesEnabled = preferences.automatic_updates
        liveboardPort = preferences.liveboard_port
        launchTunnelAtLoginItem.state = tunnelAutostartEnabled ? .on : .off
        automaticUpdatesItem.state = automaticUpdatesEnabled ? .on : .off
        liveboardPortItem.title = "Set Liveboard Port… (\(liveboardPort))"
        if !automaticUpdatesEnabled {
            pendingAutomaticUpdateVersion = nil
        }
    }

    private var validStartFolder: String? {
        let path = configuredStartFolder
        var isDirectory = ObjCBool(false)
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory), isDirectory.boolValue else {
            return nil
        }
        return path
    }

    private func updateStartFolderItems() {
        let availability = validStartFolder == nil ? " (Unavailable)" : ""
        startFolderItem.title = "Start Folder: \(displayPath(configuredStartFolder))\(availability)"
        changeFolderItem.title = "Change Start Folder…"
    }

    private func updateLaunchAtLoginItem() {
        launchAtLoginItem.state = launchAtLoginEnabled() == true ? .on : .off
    }

    private var installedMenuBarExecutableURL: URL {
        Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/zodex-menubar", isDirectory: false)
    }

    private func launchAtLoginEnabled() -> Bool? {
        // ServiceManagement resolves mainApp from the calling app bundle. Run
        // this from the current on-disk bundle so a process left alive across
        // an atomic app replacement never operates on a deleted old bundle.
        let result = Self.runCommand(
            executable: installedMenuBarExecutableURL,
            arguments: ["--login-item-status"]
        )
        guard result.exitCode == 0 else {
            return nil
        }
        return result.output.trimmingCharacters(in: .whitespacesAndNewlines) == "enabled"
    }

    private func displayPath(_ path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        if path == home {
            return "~"
        }
        let homePrefix = home + "/"
        if path.hasPrefix(homePrefix) {
            return "~/" + path.dropFirst(homePrefix.count)
        }
        return path
    }

    private func refreshStatus() {
        if upgradeInFlight {
            disableRuntimeActions()
            return
        }
        disableRuntimeActions()
        let result = runZodexSync(["local", "status", "--json"])
        guard !commandInFlight else {
            return
        }
        guard result.exitCode == 0,
              let data = result.output.data(using: .utf8),
              let status = try? JSONDecoder().decode(LocalStatus.self, from: data)
        else {
            enableFallbackActions()
            return
        }
        apply(status: status)
    }

    private func apply(status: LocalStatus) {
        currentRuntimeID = status.runtime?.runtime_id

        switch status.state {
        case "running":
            startItem.title = "Zodex is Running"
            startItem.isEnabled = false
            stopItem.isEnabled = true
            liveboardItem.isEnabled = true
            copyLiveboardItem.isEnabled = true
        case "stale":
            startItem.title = "Start Zodex"
            startItem.isEnabled = false
            stopItem.isEnabled = true
            liveboardItem.isEnabled = false
            copyLiveboardItem.isEnabled = false
        case "stopped":
            startItem.title = "Start Zodex"
            startItem.isEnabled = validStartFolder != nil
            stopItem.isEnabled = false
            liveboardItem.isEnabled = false
            copyLiveboardItem.isEnabled = false
            applyPendingAutomaticUpdateIfIdle()
        case "unconfigured":
            startItem.title = "Start Zodex"
            disableRuntimeActions()
        default:
            startItem.title = "Start Zodex"
            enableFallbackActions()
        }
    }

    private func disableRuntimeActions() {
        startItem.isEnabled = false
        stopItem.isEnabled = false
        liveboardItem.isEnabled = false
        copyLiveboardItem.isEnabled = false
    }

    private func enableFallbackActions() {
        startItem.isEnabled = validStartFolder != nil
        stopItem.isEnabled = true
        liveboardItem.isEnabled = true
        copyLiveboardItem.isEnabled = true
    }

    @objc private func startZodex() {
        guard let path = validStartFolder else {
            showError("Start folder unavailable", detail: "Choose an existing start folder before starting Zodex.")
            return
        }
        beginStart(path: path, showFailure: true)
    }

    private func beginStart(path: String, showFailure: Bool) {
        commandInFlight = true
        startItem.title = "Starting Zodex…"
        disableRuntimeActions()
        runZodexStart(path: path) { [weak self] result in
            guard let self else { return }
            self.commandInFlight = false
            if result.exitCode != 0 {
                self.startItem.title = "Start Zodex"
                if showFailure {
                    self.showCommandError("Start Zodex failed", result: result)
                } else {
                    NSLog("Zodex login tunnel startup failed: %@", result.output)
                }
            }
            self.refreshStatus()
        }
    }

    @objc private func stopZodex() {
        commandInFlight = true
        startItem.title = "Stopping Zodex…"
        disableRuntimeActions()
        runZodex(["local", "stop"]) { [weak self] result in
            guard let self else { return }
            self.commandInFlight = false
            if result.exitCode != 0 {
                self.startItem.title = "Zodex is Running"
                self.showCommandError("Stop Zodex failed", result: result)
            }
            self.refreshStatus()
        }
    }

    @objc private func openLiveboard() {
        liveboardItem.isEnabled = false
        runZodex(["local", "watch"]) { [weak self] result in
            guard let self else { return }
            self.refreshStatus()
            guard result.exitCode == 0 else {
                self.showCommandError("Open Liveboard failed", result: result)
                return
            }
        }
    }

    @objc private func copyLiveboardLink() {
        copyLiveboardItem.isEnabled = false
        runZodex(["local", "watch", "copyurl"]) { [weak self] result in
            guard let self else { return }
            self.refreshStatus()
            guard result.exitCode == 0 else {
                self.showCommandError("Copy Liveboard Link failed", result: result)
                return
            }
            self.copyLiveboardItem.title = "Liveboard Link Copied"
        }
    }

    @objc private func changeStartFolder() {
        let panel = NSOpenPanel()
        panel.title = "Change Zodex Start Folder"
        panel.prompt = "Choose"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true

        if let path = validStartFolder {
            panel.directoryURL = URL(fileURLWithPath: path, isDirectory: true)
        } else {
            panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser
        }

        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, let selectedURL = panel.url else {
            return
        }

        let path = selectedURL.standardizedFileURL.resolvingSymlinksInPath().path
        let result = runZodexSync(["local", "autostart", "folder", path])
        guard result.exitCode == 0 else {
            showCommandError("Could not save Start Folder", result: result)
            return
        }
        defaults.set(path, forKey: Self.startFolderKey)
        refreshLoginPreferences()
        updateStartFolderItems()
        refreshStatus()
    }

    @objc private func setLiveboardPort() {
        let alert = NSAlert()
        alert.messageText = "Set Liveboard Port"
        alert.informativeText = "Enter a port from 1024 to 65535. Restart Zodex Local to apply it. The default is 64973."
        let input = NSTextField(string: String(liveboardPort))
        input.frame = NSRect(x: 0, y: 0, width: 240, height: 24)
        alert.accessoryView = input
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let port = input.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let result = runZodexSync(["local", "preferences", "liveboard-port", port])
        guard result.exitCode == 0 else {
            showCommandError("Could not save Liveboard port", result: result)
            return
        }
        refreshLoginPreferences()
        showInformation("Liveboard port saved", detail: "Restart Zodex Local to use port \(liveboardPort).")
    }

    @objc private func toggleAutomaticUpdates() {
        let result = runZodexSync([
            "local", "preferences", "automatic-updates", automaticUpdatesEnabled ? "false" : "true"
        ])
        if result.exitCode != 0 {
            showCommandError("Could not change automatic updates", result: result)
        }
        refreshLoginPreferences()
        if automaticUpdatesEnabled && result.exitCode == 0 {
            checkAutomaticUpdatesIfDue()
        }
    }

    @objc private func toggleLaunchAtLogin() {
        guard let currentlyEnabled = launchAtLoginEnabled() else {
            updateLaunchAtLoginItem()
            showError(
                "Launch Menu Bar at Login could not be changed",
                detail: "The installed Zodex menu bar helper could not report its login-item status."
            )
            return
        }

        if currentlyEnabled && tunnelAutostartEnabled {
            let result = runZodexSync(["local", "autostart", "disable"])
            guard result.exitCode == 0 else {
                showCommandError("Could not disable Launch Tunnel at Login", result: result)
                updateLaunchAtLoginItem()
                return
            }
            refreshLoginPreferences()
        }

        // Use the same fresh helper for the mutation for the same reason as
        // the status query above.
        let result = Self.runCommand(
            executable: installedMenuBarExecutableURL,
            arguments: [currentlyEnabled ? "--unregister-login-item" : "--register-login-item"]
        )
        updateLaunchAtLoginItem()
        if result.exitCode != 0 {
            let detail = result.output.trimmingCharacters(in: .whitespacesAndNewlines)
            showError(
                "Launch Menu Bar at Login could not be changed",
                detail: detail.isEmpty ? "The login-item helper exited with status \(result.exitCode)." : detail
            )
        }
    }

    @objc private func toggleLaunchTunnelAtLogin() {
        let arguments: [String]
        if tunnelAutostartEnabled {
            arguments = ["local", "autostart", "disable"]
        } else {
            guard let path = validStartFolder else {
                showError("Start folder unavailable", detail: "Your home or selected Start Folder is unavailable.")
                refreshLoginPreferences()
                return
            }
            arguments = ["local", "autostart", "enable", path]
        }
        let result = runZodexSync(arguments)
        if result.exitCode != 0 {
            showCommandError("Could not change Launch Tunnel at Login", result: result)
        }
        refreshLoginPreferences()
        updateLaunchAtLoginItem()
    }

    @objc private func updateZodex() {
        if let version = availableUpdateVersion {
            beginUpgrade(version: version, stopLocal: false, automatic: false)
        } else {
            refreshUpgradeStatus(force: true, reportUpToDate: true, automatic: false)
        }
    }

    private func checkAutomaticUpdatesIfDue() {
        refreshLoginPreferences()
        guard automaticUpdatesEnabled, !upgradeCheckInFlight, !upgradeInFlight else { return }
        runZodex(["local", "preferences", "claim-update-check"]) { [weak self] result in
            guard let self, self.automaticUpdatesEnabled,
                  result.exitCode == 0,
                  let data = result.output.data(using: .utf8),
                  let claim = try? JSONDecoder().decode(AutomaticUpdateClaim.self, from: data),
                  claim.due else { return }
            self.refreshUpgradeStatus(force: true, reportUpToDate: false, automatic: true)
        }
    }

    private func applyPendingAutomaticUpdateIfIdle() {
        guard automaticUpdatesEnabled,
              !commandInFlight, !upgradeCheckInFlight, !upgradeInFlight,
              let version = pendingAutomaticUpdateVersion else { return }
        // Only the upgrade CLI can definitively decide whether Local has
        // stopped. Checking status here avoids knowingly disrupting work.
        let result = runZodexSync(["local", "status", "--json"])
        guard result.exitCode == 0,
              let data = result.output.data(using: .utf8),
              let status = try? JSONDecoder().decode(LocalStatus.self, from: data),
              status.state == "stopped" || status.state == "unconfigured" else { return }
        beginUpgrade(version: version, stopLocal: false, automatic: true)
    }

    private func refreshUpgradeStatus(force: Bool, reportUpToDate: Bool, automatic: Bool) {
        guard !upgradeCheckInFlight, !upgradeInFlight else {
            return
        }
        upgradeCheckInFlight = true
        updateItem.title = "Checking for Updates…"
        updateItem.isEnabled = false

        var arguments = ["upgrade", "--check", "--format", "json"]
        if force {
            arguments.append("--refresh")
        }
        runZodex(arguments) { [weak self] result in
            guard let self else { return }
            self.upgradeCheckInFlight = false
            guard result.exitCode == 0,
                  let event = Self.lastUpgradeEvent(named: "check_complete", in: result.output)
            else {
                self.availableUpdateVersion = nil
                self.updateItem.title = "Check for Updates…"
                self.updateItem.isEnabled = true
                if reportUpToDate {
                    let failure = Self.lastUpgradeEvent(named: "failed", in: result.output)
                    self.showError(
                        "Update check failed",
                        detail: failure?.message
                            ?? result.output.trimmingCharacters(in: .whitespacesAndNewlines)
                    )
                }
                return
            }

            self.versionItem.title = "Zodex v\(event.current_version)"
            if event.update_available == true, let target = event.target_version {
                self.availableUpdateVersion = target
                self.updateItem.title = "Update to v\(target)…"
                if automatic && self.automaticUpdatesEnabled {
                    self.pendingAutomaticUpdateVersion = target
                }
            } else {
                self.availableUpdateVersion = nil
                self.updateItem.title = "Check for Updates…"
                if automatic { self.pendingAutomaticUpdateVersion = nil }
                if reportUpToDate {
                    self.showInformation("Zodex is up to date", detail: event.message)
                }
            }
            self.updateItem.isEnabled = true
            if automatic {
                self.applyPendingAutomaticUpdateIfIdle()
            }
        }
    }

    private func beginUpgrade(version: String, stopLocal: Bool, automatic: Bool) {
        guard !upgradeInFlight else {
            return
        }
        upgradeInFlight = true
        disableRuntimeActions()
        updateItem.title = "Preparing Update…"
        updateItem.isEnabled = false

        var arguments = ["upgrade", "--version", version, "--format", "json"]
        if stopLocal {
            arguments.append("--stop-local")
        }
        runUpgradeStreaming(
            arguments: arguments,
            onEvent: { [weak self] event in
                self?.handleUpgradeEvent(event)
            },
            completion: { [weak self] exitCode, stdout, stderr in
                guard let self else { return }
                self.upgradeInFlight = false
                if exitCode == 0 {
                    self.availableUpdateVersion = nil
                    self.pendingAutomaticUpdateVersion = nil
                    self.updateItem.title = "Check for Updates…"
                    self.updateItem.isEnabled = true
                    self.refreshStatus()
                    return
                }

                let failure = Self.lastUpgradeEvent(named: "failed", in: stdout)
                if failure?.code == "local_running", !stopLocal {
                    self.updateItem.title = "Update to v\(version)…"
                    self.updateItem.isEnabled = true
                    if automatic {
                        self.pendingAutomaticUpdateVersion = version
                    } else {
                        self.confirmStopAndUpdate(version: version)
                    }
                    self.refreshStatus()
                    return
                }

                self.updateItem.title = "Update to v\(version)…"
                self.updateItem.isEnabled = true
                self.refreshStatus()
                let detail = failure?.message
                    ?? stderr.trimmingCharacters(in: .whitespacesAndNewlines)
                if automatic {
                    self.pendingAutomaticUpdateVersion = nil
                    NSLog("Zodex automatic update failed: %@", detail)
                } else {
                    self.showError(
                        "Zodex update failed",
                        detail: detail.isEmpty ? "The upgrade command exited with status \(exitCode)." : detail
                    )
                }
            }
        )
    }

    private func handleUpgradeEvent(_ event: UpgradeEvent) {
        switch event.event {
        case "stopping_local":
            updateItem.title = "Stopping Zodex…"
        case "downloading", "retrying_download":
            updateItem.title = event.event == "retrying_download" ? "Retrying Download…" : "Downloading Update…"
        case "verifying":
            updateItem.title = "Verifying Update…"
        case "installing":
            updateItem.title = "Installing Update…"
        case "complete":
            updateItem.title = "Updated to v\(event.target_version ?? event.current_version)"
        default:
            break
        }
    }

    private func confirmStopAndUpdate(version: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Stop Zodex Local and update?"
        alert.informativeText = "Zodex Local must stop before the operator can be replaced. Active Zodex-owned processes will be stopped, then Zodex will update to v\(version)."
        alert.addButton(withTitle: "Stop and Update")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            beginUpgrade(version: version, stopLocal: true, automatic: false)
        }
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }

    private func runZodex(_ arguments: [String], completion: @escaping (CommandResult) -> Void) {
        let executable = zodexURL
        DispatchQueue.global(qos: .userInitiated).async {
            let result = Self.runCommand(executable: executable, arguments: arguments)
            DispatchQueue.main.async {
                completion(result)
            }
        }
    }

    private func runZodexStart(path: String, completion: @escaping (CommandResult) -> Void) {
        let executable = zodexURL
        DispatchQueue.global(qos: .userInitiated).async {
            guard let environment = captureLoginShellEnvironment() else {
                DispatchQueue.main.async {
                    completion(CommandResult(
                        exitCode: -1,
                        output: "Could not capture the interactive login-shell environment for Start Zodex."
                    ))
                }
                return
            }
            let process = Process()
            let outputPipe = Pipe()
            process.executableURL = executable
            process.arguments = ["local", "start", path]
            process.environment = environment
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = outputPipe
            process.standardError = outputPipe

            let result: CommandResult
            do {
                try process.run()
                let data = outputPipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                result = CommandResult(
                    exitCode: process.terminationStatus,
                    output: String(decoding: data, as: UTF8.self)
                )
            } catch {
                result = CommandResult(exitCode: -1, output: error.localizedDescription)
            }

            DispatchQueue.main.async {
                completion(result)
            }
        }
    }

    private func runZodexSync(_ arguments: [String]) -> CommandResult {
        Self.runCommand(executable: zodexURL, arguments: arguments)
    }

    private static func runCommand(executable: URL, arguments: [String]) -> CommandResult {
        let process = Process()
        let outputPipe = Pipe()
        process.executableURL = executable
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = outputPipe
        process.standardError = outputPipe

        do {
            try process.run()
            let data = outputPipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return CommandResult(
                exitCode: process.terminationStatus,
                output: String(decoding: data, as: UTF8.self)
            )
        } catch {
            return CommandResult(exitCode: -1, output: error.localizedDescription)
        }
    }

    private func runUpgradeStreaming(
        arguments: [String],
        onEvent: @escaping (UpgradeEvent) -> Void,
        completion: @escaping (Int32, String, String) -> Void
    ) {
        let executable = zodexURL
        let process = Process()
        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.executableURL = executable
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        do {
            try process.run()
        } catch {
            completion(-1, "", error.localizedDescription)
            return
        }

        DispatchQueue.global(qos: .userInitiated).async {
            let stdoutHandle = stdoutPipe.fileHandleForReading
            var stdout = ""
            var pending = ""
            while true {
                let data = stdoutHandle.availableData
                if data.isEmpty {
                    break
                }
                let chunk = String(decoding: data, as: UTF8.self)
                stdout += chunk
                pending += chunk
                while let newline = pending.firstIndex(of: "\n") {
                    let line = String(pending[..<newline])
                    pending.removeSubrange(...newline)
                    guard let data = line.data(using: .utf8),
                          let event = try? JSONDecoder().decode(UpgradeEvent.self, from: data)
                    else {
                        continue
                    }
                    DispatchQueue.main.async {
                        onEvent(event)
                    }
                }
            }
            process.waitUntilExit()
            let stderr = String(
                decoding: stderrPipe.fileHandleForReading.readDataToEndOfFile(),
                as: UTF8.self
            )
            DispatchQueue.main.async {
                completion(process.terminationStatus, stdout, stderr)
            }
        }
    }

    private static func lastUpgradeEvent(named name: String, in output: String) -> UpgradeEvent? {
        for line in output.split(whereSeparator: \.isNewline).reversed() {
            guard let data = String(line).data(using: .utf8),
                  let event = try? JSONDecoder().decode(UpgradeEvent.self, from: data),
                  event.event == name
            else {
                continue
            }
            return event
        }
        return nil
    }

    private func showCommandError(_ title: String, result: CommandResult) {
        let detail = result.output.trimmingCharacters(in: .whitespacesAndNewlines)
        showError(title, detail: detail.isEmpty ? "Zodex exited with status \(result.exitCode)." : detail)
    }

    private func showError(_ title: String, detail: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    private func showInformation(_ title: String, detail: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.alertStyle = .informational
        alert.messageText = title
        alert.informativeText = detail
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }
}
