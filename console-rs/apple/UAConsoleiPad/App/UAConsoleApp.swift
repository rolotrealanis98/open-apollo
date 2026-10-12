import SwiftUI

/// iPad host for console-rs: a CAMetalLayer-backed view driven by the Rust renderer through
/// the C API in `uaconsole.h`. UA's art comes from a Resources folder the user imports.
@main
struct UAConsoleApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}

struct RootView: View {
    @State private var resourcesPath: String? = ResourceStore.locate()
    @State private var compact = true
    @State private var status = ""
    @State private var fitRequests = 0
    @State private var generation = 0

    var body: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            if let resourcesPath {
                ConsoleSurface(resourcesPath: resourcesPath, compact: compact, fitRequests: fitRequests) { status = $0 }
                    .id(generation) // a fresh renderer after each import
                    .ignoresSafeArea()
            } else {
                VStack(spacing: 16) {
                    Text("No UAD Console resources")
                        .font(.title2)
                    Text("Import the Resources folder of an installed UAD Console (on a Mac: UAD Console.app → Contents → Resources). Nothing from UA is bundled with this app.")
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: 520)
                    ResourceImportButton(resourcesPath: $resourcesPath)
                }
                .padding(.top, 160)
            }
            HStack(spacing: 12) {
                Text(status)
                    .font(.caption.monospaced())
                    .foregroundStyle(.white.opacity(0.75))
                    .lineLimit(1)
                if resourcesPath != nil {
                    Button("Fit") { fitRequests += 1 }
                    Toggle("Compact", isOn: $compact)
                        .toggleStyle(.button)
                    ResourceImportButton(resourcesPath: $resourcesPath, compactLabel: true) { generation += 1 }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(.black.opacity(0.55), in: .rect(cornerRadius: 8))
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .preferredColorScheme(.dark)
    }
}
