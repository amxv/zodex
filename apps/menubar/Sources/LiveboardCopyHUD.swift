import AppKit
import Darwin
import Foundation

enum LiveboardCopyHUDError: Error {
    case invalidAgent
    case invalidInput
    case invalidURL
    case clipboardUnavailable
}

func copyFocusedLiveboardLinkAndShowHUD(agentID: String) throws {
    guard agentID.utf8.count == 4,
          agentID.utf8.allSatisfy({ byte in
              (byte >= 97 && byte <= 122) || (byte >= 48 && byte <= 57)
          })
    else {
        throw LiveboardCopyHUDError.invalidAgent
    }

    let raw = try readBoundedStandardInput()
    guard let value = String(data: raw, encoding: .utf8),
          let components = URLComponents(string: value),
          components.scheme == "http",
          components.host == "127.0.0.1",
          components.port != nil,
          components.user == nil,
          components.password == nil,
          components.fragment == nil,
          components.path == "/",
          components.queryItems == [URLQueryItem(name: "agent", value: agentID)]
    else {
        throw LiveboardCopyHUDError.invalidURL
    }

    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    guard pasteboard.setString(value, forType: .string) else {
        throw LiveboardCopyHUDError.clipboardUnavailable
    }
    showFocusedLinkHUD(agentID: agentID)
}

private func readBoundedStandardInput() throws -> Data {
    var bytes = [UInt8](repeating: 0, count: 4097)
    let count = bytes.withUnsafeMutableBytes { buffer in
        Darwin.read(STDIN_FILENO, buffer.baseAddress, buffer.count)
    }
    guard count > 0, count <= 4096 else {
        throw LiveboardCopyHUDError.invalidInput
    }
    var trailing: UInt8 = 0
    let trailingCount = withUnsafeMutablePointer(to: &trailing) { pointer in
        Darwin.read(STDIN_FILENO, pointer, 1)
    }
    guard trailingCount == 0 else {
        throw LiveboardCopyHUDError.invalidInput
    }
    return Data(bytes.prefix(count))
}

private func showFocusedLinkHUD(agentID: String) {
    guard let screen = NSScreen.main else { return }

    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let size = NSSize(width: 330, height: 54)
    let panel = NSPanel(
        contentRect: NSRect(origin: .zero, size: size),
        styleMask: [.borderless, .nonactivatingPanel],
        backing: .buffered,
        defer: false
    )
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true
    panel.level = .floating
    panel.collectionBehavior = [.canJoinAllSpaces, .transient, .ignoresCycle]
    panel.hidesOnDeactivate = false

    let effect = NSVisualEffectView(frame: NSRect(origin: .zero, size: size))
    effect.material = .hudWindow
    effect.blendingMode = .behindWindow
    effect.state = .active
    effect.wantsLayer = true
    effect.layer?.cornerRadius = 13
    effect.layer?.masksToBounds = true

    let check = NSTextField(labelWithString: "✓")
    check.alignment = .center
    check.font = NSFont.systemFont(ofSize: 16, weight: .bold)
    check.textColor = .systemGreen
    check.translatesAutoresizingMaskIntoConstraints = false
    effect.addSubview(check)

    let label = NSTextField(labelWithString: "Focused Liveboard link copied · Agent \(agentID)")
    label.alignment = .center
    label.font = NSFont.systemFont(ofSize: 13, weight: .medium)
    label.textColor = .labelColor
    label.translatesAutoresizingMaskIntoConstraints = false
    effect.addSubview(label)
    NSLayoutConstraint.activate([
        check.leadingAnchor.constraint(equalTo: effect.leadingAnchor, constant: 16),
        check.centerYAnchor.constraint(equalTo: effect.centerYAnchor),
        check.widthAnchor.constraint(equalToConstant: 18),
        label.leadingAnchor.constraint(equalTo: check.trailingAnchor, constant: 8),
        label.trailingAnchor.constraint(equalTo: effect.trailingAnchor, constant: -16),
        label.centerYAnchor.constraint(equalTo: effect.centerYAnchor),
    ])
    panel.contentView = effect

    let frame = screen.visibleFrame
    panel.setFrameOrigin(NSPoint(
        x: frame.midX - size.width / 2,
        y: frame.maxY - size.height - 26
    ))
    panel.alphaValue = 0
    panel.orderFrontRegardless()
    NSAnimationContext.runAnimationGroup { context in
        context.duration = 0.12
        panel.animator().alphaValue = 1
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) {
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.2
            panel.animator().alphaValue = 0
        } completionHandler: {
            panel.orderOut(nil)
            app.terminate(nil)
        }
    }
    app.run()
}
