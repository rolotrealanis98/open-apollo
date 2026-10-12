import QuartzCore
import SwiftUI
import UAConsoleFFI
import UIKit

/// Owns one `UacContext` (Rust: houston + rack + gfx/wgpu) and destroys it with itself.
final class ConsoleContext {
    let raw: OpaquePointer

    init(resourcesPath: String, layer: CAMetalLayer, widthPx: UInt32, heightPx: UInt32, scale: Float) throws(ConsoleError) {
        let layerPtr = Unmanaged.passUnretained(layer).toOpaque()
        guard let raw = resourcesPath.withCString({ uac_create($0, layerPtr, widthPx, heightPx, scale) }) else {
            throw ConsoleError(message: String(cString: uac_last_error()))
        }
        self.raw = raw
    }

    deinit {
        uac_destroy(raw)
    }

    var stats: UacStats {
        var s = UacStats()
        _ = uac_get_stats(raw, &s)
        return s
    }

    var adapterName: String {
        String(cString: uac_adapter_name(raw))
    }
}

struct ConsoleError: Error {
    let message: String
}

struct Bench {
    let frames: Int
    private(set) var remaining: Int
    private var build = 0.0, render = 0.0, wall = 0.0, last: Date?

    init(frames: Int) {
        self.frames = max(0, frames)
        remaining = self.frames
    }

    mutating func add(_ s: UacStats) {
        let now = Date()
        if let last { wall += now.timeIntervalSince(last) }
        last = now
        build += s.build_ms
        render += s.render_ms
        remaining -= 1
    }

    var summary: String {
        let n = Double(max(frames, 1))
        return String(format: "bench %d warm frames: build %.3f ms, render %.3f ms (CPU avg), %.1f fps paced by the display link", frames, build / n, render / n, Double(max(frames - 1, 1)) / max(wall, 1e-9))
    }
}

/// A UIView whose layer is a CAMetalLayer that the Rust renderer draws into. Frames are
/// driven by a CADisplayLink and only rendered when the context reports a change.
final class ConsoleMetalView: UIView {
    override class var layerClass: AnyClass { CAMetalLayer.self }
    private var metalLayer: CAMetalLayer { layer as! CAMetalLayer }

    private var context: ConsoleContext?
    private var link: CADisplayLink?
    private var activeTouch: UITouch?
    private var pinchStartZoom: Float = 0
    private var lastStatus = Date.distantPast
    private var loggedFirstFrame = false
    /// `UAC_BENCH_FRAMES=N` (simulator: `SIMCTL_CHILD_UAC_BENCH_FRAMES`) redraws N warm frames
    /// after the first one and logs the averages.
    private var bench = Bench(frames: Int(ProcessInfo.processInfo.environment["UAC_BENCH_FRAMES"] ?? "") ?? 0)

    var onStatus: ((String) -> Void)?

    var resourcesPath = "" {
        didSet {
            guard resourcesPath != oldValue else { return }
            context = nil
            setNeedsLayout()
        }
    }

    var compact = true {
        didSet {
            if let context, compact != oldValue { uac_set_compact(context.raw, compact) }
        }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        // The layer's contents can be discarded in the background; redraw on return.
        NotificationCenter.default.addObserver(self, selector: #selector(invalidate), name: UIApplication.willEnterForegroundNotification, object: nil)
        isOpaque = true
        backgroundColor = .black
        addGestureRecognizer(UIPinchGestureRecognizer(target: self, action: #selector(pinch(_:))))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    @objc private func invalidate() {
        if let context { uac_invalidate(context.raw) }
    }

    func fitToView() {
        if let context { uac_set_zoom(context.raw, 0, 0, 0) }
    }

    // MARK: lifecycle

    private var drawable: (width: UInt32, height: UInt32, scale: Float) {
        let scale = traitCollection.displayScale
        return (UInt32(max(1, (bounds.width * scale).rounded())), UInt32(max(1, (bounds.height * scale).rounded())), Float(scale))
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard window != nil, bounds.width > 0, bounds.height > 0, !resourcesPath.isEmpty else { return }
        let d = drawable
        metalLayer.contentsScale = CGFloat(d.scale)
        if let context {
            uac_resize(context.raw, d.width, d.height, d.scale)
        } else {
            do {
                let ctx = try ConsoleContext(resourcesPath: resourcesPath, layer: metalLayer, widthPx: d.width, heightPx: d.height, scale: d.scale)
                uac_set_compact(ctx.raw, compact)
                context = ctx
                report(ctx.adapterName)
            } catch {
                report("uac_create failed: \(error.message)")
            }
        }
        startDisplayLink()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window == nil {
            // The display link retains its target; break the cycle when leaving the window.
            link?.invalidate()
            link = nil
        } else {
            invalidate()
            setNeedsLayout()
        }
    }

    /// Status from layout runs inside SwiftUI's update pass; hand it over on the next turn.
    private func report(_ message: String) {
        Task { @MainActor [weak self] in self?.onStatus?(message) }
    }

    private func startDisplayLink() {
        guard link == nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(step(_:)))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    @objc private func step(_ link: CADisplayLink) {
        guard let context, uac_needs_frame(context.raw) else { return }
        let rc = uac_frame(context.raw)
        if rc < 0 {
            onStatus?("frame error \(rc): \(String(cString: uac_last_error()))")
            return
        }
        guard rc == UAC_OK else { return }
        let s = context.stats
        if !loggedFirstFrame {
            loggedFirstFrame = true
            log("first frame on \(context.adapterName): \(Self.describe(s))")
        } else if bench.remaining > 0 {
            bench.add(s)
            if bench.remaining == 0 { log(bench.summary) }
        }
        if bench.remaining > 0 { uac_invalidate(context.raw) }
        if Date().timeIntervalSince(lastStatus) > 0.5 {
            lastStatus = Date()
            onStatus?(Self.describe(s))
        }
    }

    /// Unbuffered (stderr), so `simctl launch --stderr=` sees it while the app runs.
    private func log(_ line: String) {
        FileHandle.standardError.write(Data("uaconsole: \(line)\n".utf8))
    }

    static func describe(_ s: UacStats) -> String {
        String(
            format: "#%llu build %.2f ms · render %.2f ms · %u quads · %u batches · %u tex (%.1f MB) · %u glyphs · zoom %.2f",
            s.frames, s.build_ms, s.render_ms, s.quads, s.batches, s.textures, Double(s.texture_bytes) / 1e6, s.glyphs, s.zoom
        )
    }

    // MARK: input (one tracked touch → uac_pointer; pinch → uac_set_zoom)

    private func send(_ kind: Int32, _ touch: UITouch, flags: UInt32 = 0) {
        guard let context else { return }
        let p = touch.location(in: self)
        _ = uac_pointer(context.raw, kind, Float(p.x), Float(p.y), flags)
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard activeTouch == nil, let touch = touches.first else { return }
        activeTouch = touch
        send(UAC_POINTER_DOWN, touch, flags: touch.tapCount >= 2 ? UAC_POINTER_DOUBLE : 0)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = activeTouch, touches.contains(touch) else { return }
        send(UAC_POINTER_MOVE, touch)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = activeTouch, touches.contains(touch) else { return }
        send(UAC_POINTER_UP, touch)
        activeTouch = nil
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let touch = activeTouch, touches.contains(touch) else { return }
        send(UAC_POINTER_CANCEL, touch)
        activeTouch = nil
    }

    @objc private func pinch(_ g: UIPinchGestureRecognizer) {
        guard let context else { return }
        let p = g.location(in: self)
        switch g.state {
        case .began:
            pinchStartZoom = context.stats.zoom
        case .changed:
            uac_set_zoom(context.raw, pinchStartZoom * Float(g.scale), Float(p.x), Float(p.y))
        default:
            break
        }
    }
}

/// SwiftUI wrapper.
struct ConsoleSurface: UIViewRepresentable {
    let resourcesPath: String
    let compact: Bool
    /// Bumped by the host to return to fit-to-view.
    let fitRequests: Int
    let onStatus: (String) -> Void

    final class Coordinator {
        var fitRequests = 0
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> ConsoleMetalView {
        let view = ConsoleMetalView(frame: .zero)
        context.coordinator.fitRequests = fitRequests
        update(view)
        return view
    }

    func updateUIView(_ view: ConsoleMetalView, context: Context) {
        update(view)
        if context.coordinator.fitRequests != fitRequests {
            context.coordinator.fitRequests = fitRequests
            view.fitToView()
        }
    }

    private func update(_ view: ConsoleMetalView) {
        view.onStatus = onStatus
        view.compact = compact
        view.resourcesPath = resourcesPath
    }
}
