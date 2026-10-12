import Foundation
import SwiftUI
import UAConsoleFFI
import UniformTypeIdentifiers

/// Where the app finds UA's Console art. Nothing is bundled: the user imports a UAD Console
/// `Resources` folder from Files, and it is copied into Documents/UADConsoleResources.
/// (Same idea as ConsoleLab's ResourceImport.swift.)
enum ResourceStore {
    static let importedFolderName = "UADConsoleResources"

    static var importedURL: URL? {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?
            .appending(path: importedFolderName, directoryHint: .isDirectory)
    }

    static func isResources(_ url: URL) -> Bool {
        uac_resources_valid(url.path(percentEncoded: false))
    }

    /// `$CONSOLE_RESOURCES` (simulator: `SIMCTL_CHILD_CONSOLE_RESOURCES`), the imported copy,
    /// then a `Resources` folder dropped into Documents through Finder file sharing.
    static func locate() -> String? {
        var candidates: [URL] = []
        if let env = ProcessInfo.processInfo.environment["CONSOLE_RESOURCES"], !env.isEmpty {
            candidates.append(URL(filePath: env, directoryHint: .isDirectory))
        }
        if let imported = importedURL {
            candidates.append(imported)
            candidates.append(imported.deletingLastPathComponent().appending(path: "Resources", directoryHint: .isDirectory))
        }
        return candidates.first(where: isResources)?.path(percentEncoded: false)
    }

    /// Copies the picked folder (the Resources folder itself, the .app, or its Contents) into
    /// Documents. Only what console-rs reads: appearance, layouts, images, fonts, plug-in icons.
    static func copy(from picked: URL) -> (ok: Bool, message: String) {
        let scoped = picked.startAccessingSecurityScopedResource()
        defer { if scoped { picked.stopAccessingSecurityScopedResource() } }
        let candidates = [picked, picked.appending(path: "Resources"), picked.appending(path: "Contents/Resources")]
        guard let source = candidates.first(where: isResources) else {
            return (false, "That folder has no appearance/Appearance.json")
        }
        guard let target = importedURL else { return (false, "No Documents folder") }
        let fm = FileManager.default
        do {
            if fm.fileExists(atPath: target.path(percentEncoded: false)) {
                try fm.removeItem(at: target)
            }
            try fm.createDirectory(at: target, withIntermediateDirectories: true)
            for sub in ["appearance", "layouts", "images", "fonts", "Plug-In Icons"] {
                let from = source.appending(path: sub)
                if fm.fileExists(atPath: from.path(percentEncoded: false)) {
                    try fm.copyItem(at: from, to: target.appending(path: sub))
                }
            }
            // Console's version string lives in the bundle's Info.plist next to Resources.
            let info = source.deletingLastPathComponent().appending(path: "Info.plist")
            if fm.fileExists(atPath: info.path(percentEncoded: false)) {
                try? fm.copyItem(at: info, to: target.appending(path: "Info.plist"))
            }
            return (true, "Imported from \(source.lastPathComponent)")
        } catch {
            return (false, error.localizedDescription)
        }
    }
}

struct ResourceImportButton: View {
    @Binding var resourcesPath: String?
    var compactLabel = false
    /// Called after a successful import (the path may be unchanged, but the files are new).
    var onImported: () -> Void = {}
    @State private var picking = false
    @State private var status: String?

    var body: some View {
        VStack(alignment: .center, spacing: 6) {
            Button(compactLabel ? "Import…" : "Import UAD Console Resources…") { picking = true }
                .buttonStyle(.bordered)
            if let status, !compactLabel {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
        }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { result in
            switch result {
            case let .success(url):
                status = "Copying…"
                Task {
                    // Copy off the main actor; only the Sendable URL crosses over.
                    let outcome = await Task.detached(priority: .userInitiated) { ResourceStore.copy(from: url) }.value
                    status = outcome.message
                    if outcome.ok {
                        resourcesPath = ResourceStore.locate()
                        onImported()
                    }
                }
            case let .failure(error):
                status = error.localizedDescription
            }
        }
    }
}
