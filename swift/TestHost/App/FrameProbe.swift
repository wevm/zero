import Observation
import SwiftUI

#if os(iOS)
  import UIKit
  typealias PlatformView = UIView
  typealias PlatformScrollView = UIScrollView
#else
  import AppKit
  typealias PlatformView = NSView
  typealias PlatformScrollView = NSScrollView
#endif

/// Checks every display frame for a wrong position: with `-probe`, each row
/// carries a marker view, and on every frame the probe reads where the rows
/// are on screen. A frame is wrong when the rows move by anything but the
/// scroll that drove them: a page loading, space changing, or a jump drawn
/// at a position the list never meant to show.
///
/// - Driven by the probe itself (`drive`), the scroll is known exactly: the
///   rows must move by it, to the half point.
/// - Flung by a finger (iOS), the momentum is smooth: a frame whose speed
///   departs from both of its neighbors' is a jump in it.
/// - At rest, nothing moves.
/// - A jump (Latest) shows the rows as they were, then as they end up, and
///   nothing in between.
///
/// Every frame also checks that the rows move together. The summary is the
/// `probe` element's value, for the UI tests.
@MainActor @Observable
final class FrameProbe {
  static let shared = FrameProbe()
  static let enabled = ProcessInfo.processInfo.arguments.contains("-probe")
  static var script: String? { UserDefaults.standard.string(forKey: "probe-script") }

  /// The summary the UI tests read.
  private(set) var summary = "wrong=0"

  @ObservationIgnored private var markers: [String: WeakView] = [:]
  @ObservationIgnored private(set) weak var scrollView: PlatformScrollView?
  @ObservationIgnored private var previous: Sample?
  @ObservationIgnored private var smooth: [(v: Double, real: Bool, sample: Sample, dy: Double)] =
    []
  @ObservationIgnored private var applied: Double?
  @ObservationIgnored private var drive: (velocity: Double, done: CheckedContinuation<Void, Never>)?
  @ObservationIgnored private var jump: [Sample]?
  @ObservationIgnored private var jumpStill = 0.0
  @ObservationIgnored private var flingStart: (t: Double, moved: Double)?
  @ObservationIgnored private(set) var phase = "start"
  @ObservationIgnored private var wrongByPhase: [(String, Int)] = []
  @ObservationIgnored private(set) var wrong = 0
  @ObservationIgnored private var frames = 0
  @ObservationIgnored private var flings: [String] = []
  @ObservationIgnored private var done = false
  @ObservationIgnored private var ticker: AnyObject?

  struct Sample {
    var t: Double
    /// Rows on screen: top and bottom from the viewport's top.
    var rows: [String: (minY: Double, maxY: Double)]
    var tracking = false
    var decelerating = false
    var atEdge = false

    /// Top to bottom.
    var ids: [String] { rows.sorted { $0.value.minY < $1.value.minY }.map(\.key) }
  }

  /// The rows on screen in the last frame, top to bottom.
  var visible: [String] { previous?.ids ?? [] }
  func top(of id: String) -> Double? { previous?.rows[id]?.minY }
  func bottom(of id: String) -> Double? { previous?.rows[id]?.maxY }
  var viewportHeight: Double {
    #if os(iOS)
      guard let s = scrollView else { return 0 }
      return s.bounds.height - s.adjustedContentInset.top - s.adjustedContentInset.bottom
    #else
      return Double(scrollView?.contentView.bounds.height ?? 0)
    #endif
  }

  func register(_ id: String, _ view: PlatformView) {
    markers[id] = WeakView(view: view)
    if scrollView == nil { scrollView = Self.enclosingScrollView(view) }
    if ticker == nil, let scrollView { start(scrollView) }
  }

  func unregister(_ id: String, _ view: PlatformView) {
    if markers[id]?.view === view { markers[id] = nil }
  }

  func setPhase(_ name: String) {
    finishJump()
    log("PHASE \(name)")
    phase = name
    if !wrongByPhase.contains(where: { $0.0 == name }) { wrongByPhase.append((name, 0)) }
    publish()
  }

  func finish() {
    finishJump()
    done = true
    log("DONE \(summary)")
    publish()
  }

  /// A programmatic jump starts: the frames until it settles are checked
  /// as a jump.
  func beginJump() {
    guard Self.enabled else { return }
    finishJump()
    jump = previous.map { [$0] } ?? []
    jumpStill = 0
    log("JUMP begin top=\(previous?.ids.first ?? "-")")
  }

  /// Scrolls as a fling would: `velocity` points per second toward the
  /// content's end (negative: toward its start), decaying as UIKit's does.
  func drive(velocity: Double) async {
    await withCheckedContinuation { drive = (velocity, $0) }
  }

  // MARK: Frames

  private func start(_ view: PlatformView) {
    #if os(iOS)
      let link = CADisplayLink(target: Tick(self), selector: #selector(Tick.tick))
      link.preferredFrameRateRange = CAFrameRateRange(minimum: 80, maximum: 120, preferred: 120)
      link.add(to: .main, forMode: .common)
      ticker = link
    #else
      let link = view.displayLink(target: Tick(self), selector: #selector(Tick.tick))
      link.add(to: .main, forMode: .common)
      ticker = link
    #endif
  }

  fileprivate func tick(_ t: Double) {
    guard let scrollView else { return }
    if scrollView.window == nil { return }
    let sample = sample(t, scrollView)
    frames += 1
    if let previous { check(previous, sample) }
    previous = sample
    step(t, scrollView)
  }

  private func sample(_ t: Double, _ scrollView: PlatformScrollView) -> Sample {
    var s = Sample(t: t, rows: [:])
    #if os(iOS)
      let port = scrollView.convert(scrollView.bounds, to: nil)
        .inset(by: scrollView.adjustedContentInset)
      for (id, m) in markers {
        guard let v = m.view, v.window != nil, v.bounds.height > 0 else { continue }
        let r = v.convert(v.bounds, to: nil)
        guard r.maxY > port.minY + 0.5, r.minY < port.maxY - 0.5 else { continue }
        s.rows[id] = (Double(r.minY - port.minY), Double(r.maxY - port.minY))
      }
      s.tracking = scrollView.isTracking
      s.decelerating = scrollView.isDecelerating
      let y = scrollView.contentOffset.y + scrollView.adjustedContentInset.top
      s.atEdge = y <= 0.5 || y >= scrollView.contentSize.height - viewportHeight - 0.5
    #else
      let clip = scrollView.contentView
      let port = clip.convert(clip.bounds, to: nil)
      for (id, m) in markers {
        guard let v = m.view, v.window != nil, v.bounds.height > 0 else { continue }
        let r = v.convert(v.bounds, to: nil)
        // Window coordinates count up from the bottom.
        let minY = port.maxY - r.maxY
        let maxY = port.maxY - r.minY
        guard maxY > 0.5, minY < port.height - 0.5 else { continue }
        s.rows[id] = (Double(minY), Double(maxY))
      }
      let y = clip.bounds.origin.y
      let height = scrollView.documentView?.frame.height ?? 0
      s.atEdge = y <= 0.5 || y >= height - clip.bounds.height - 0.5
    #endif
    return s
  }

  private func check(_ a: Sample, _ b: Sample) {
    let driven = applied
    applied = nil
    if jump != nil {
      jump!.append(b)
      if moved(a, b) {
        jumpStill = 0
      } else {
        jumpStill += b.t - a.t
      }
      if jumpStill >= 0.5 { finishJump() }
      return
    }
    let common = a.rows.keys.filter { b.rows[$0] != nil }
    guard !common.isEmpty else {
      if !a.rows.isEmpty && !b.rows.isEmpty { flag(b, "no row in both frames", a) }
      return
    }
    let deltas = common.map { (id: $0, dy: b.rows[$0]!.minY - a.rows[$0]!.minY) }
    let spread = (deltas.map(\.dy).max() ?? 0) - (deltas.map(\.dy).min() ?? 0)
    if spread > 1 { flag(b, "rows moved apart by \(fmt(spread))", a) }
    // The anchor: the top row on screen in both frames.
    let anchor = common.min { a.rows[$0]!.minY < a.rows[$1]!.minY }!
    let dy = b.rows[anchor]!.minY - a.rows[anchor]!.minY
    let real = b.tracking || b.decelerating || a.tracking || a.decelerating
    if let driven {
      if abs(dy + driven) > 0.75 {
        flag(b, "\(anchor) moved \(fmt(dy)), the drive \(fmt(-driven))", a)
      }
    } else if real {
      smoothCheck(dy: dy, a: a, b: b)
    } else if abs(dy) > 0.5 {
      flag(b, "\(anchor) moved \(fmt(dy)) at rest", a)
    }
    trackFling(a, b, dy)
  }

  /// Momentum is smooth: a frame whose speed departs from both neighbors'
  /// is a jump. Checked one frame late, once the next frame is in.
  private func smoothCheck(dy: Double, a: Sample, b: Sample) {
    let dt = max(0.001, b.t - a.t)
    smooth.append((dy / dt, !(a.tracking || b.tracking), b, dy))
    if smooth.count > 3 { smooth.removeFirst() }
    guard smooth.count == 3, smooth.allSatisfy(\.real) else { return }
    let (p, m, n) = (smooth[0], smooth[1], smooth[2])
    let limit = 300.0
    if abs(m.v - p.v) > limit && abs(m.v - n.v) > limit {
      flag(m.sample, "speed \(fmt(m.v)) between \(fmt(p.v)) and \(fmt(n.v)) (moved \(fmt(m.dy)))")
    }
  }

  private func trackFling(_ a: Sample, _ b: Sample, _ dy: Double) {
    if b.decelerating && !b.tracking {
      if flingStart == nil { flingStart = (b.t, 0) }
      flingStart!.moved += dy
    } else if let start = flingStart, !b.decelerating {
      flingStart = nil
      let entry = "\(fmt(b.t - start.t))s/\(fmt(abs(start.moved)))pt\(b.atEdge ? "/edge" : "")"
      flings.append(entry)
      log("FLING \(entry)")
      publish()
    }
  }

  private func finishJump() {
    guard let frames = jump, let first = frames.first, let last = frames.last else {
      jump = nil
      return
    }
    jump = nil
    var bad = 0
    // Each state the jump showed, once.
    for (i, f) in frames.enumerated() where i == 0 || !same(f, frames[i - 1]) {
      let top = f.ids.first.map { "\($0)@\(fmt(f.rows[$0]!.minY))" } ?? "-"
      let ms = fmt((f.t - first.t) * 1000)
      log("JUMP frame \(i) +\(ms)ms \(top)…\(f.ids.last ?? "-") rows=\(f.rows.count)")
    }
    for f in frames.dropFirst() where !same(f, first) && !same(f, last) {
      bad += 1
      flag(f, "jump frame neither where it was (\(first.ids.first ?? "-")) nor where it lands")
    }
    log(
      "JUMP end frames=\(frames.count) wrong=\(bad) from \(first.ids.first ?? "-")…\(first.ids.last ?? "-") to \(last.ids.first ?? "-")…\(last.ids.last ?? "-")"
    )
  }

  private func moved(_ a: Sample, _ b: Sample) -> Bool { !same(a, b) }

  private func same(_ a: Sample, _ b: Sample) -> Bool {
    guard Set(a.rows.keys) == Set(b.rows.keys) else { return false }
    return a.rows.allSatisfy { abs($0.value.minY - b.rows[$0.key]!.minY) <= 0.5 }
  }

  private func flag(_ s: Sample, _ why: String, _ before: Sample? = nil) {
    wrong += 1
    if let i = wrongByPhase.firstIndex(where: { $0.0 == phase }) { wrongByPhase[i].1 += 1 }
    let rows = s.ids.prefix(2).map { "\($0)@\(fmt(s.rows[$0]!.minY))" }.joined(separator: " ")
    let was = before.map { b in
      " was " + b.ids.prefix(2).map { "\($0)@\(fmt(b.rows[$0]!.minY))" }.joined(separator: " ")
    }
    log("WRONG [\(phase)] \(why): \(rows)\(was ?? "")")
    publish()
  }

  private func publish() {
    let phases = wrongByPhase.map { "\($0.0):\($0.1)" }.joined(separator: ",")
    summary =
      "wrong=\(wrong) frames=\(frames) phases=[\(phases)] flings=[\(flings.suffix(8).joined(separator: ","))]\(done ? " done" : "")"
  }

  private func log(_ s: String) {
    print("PROBE \(String(format: "%.3f", Date().timeIntervalSince1970)) \(s)")
    fflush(stdout)
  }

  private func fmt(_ x: Double) -> String { String(format: "%.1f", x) }

  // MARK: Driving

  /// Moves the scroll view by this frame's share of the drive, after the
  /// frame was checked: the next frame must show the rows moved by it.
  private func step(_ t: Double, _ scrollView: PlatformScrollView) {
    guard let d = drive else { return }
    let dt = 1.0 / 120
    let move = d.velocity * dt
    #if os(iOS)
      let inset = scrollView.adjustedContentInset
      let lo = -inset.top
      let hi = max(lo, scrollView.contentSize.height + inset.bottom - scrollView.bounds.height)
      let from = scrollView.contentOffset.y
      let to = min(hi, max(lo, from + move))
      scrollView.contentOffset.y = to
    #else
      let clip = scrollView.contentView
      let height = scrollView.documentView?.frame.height ?? 0
      let from = clip.bounds.origin.y
      let to = min(max(0, height - clip.bounds.height), max(0, from + move))
      clip.scroll(to: CGPoint(x: clip.bounds.origin.x, y: to))
      scrollView.reflectScrolledClipView(clip)
    #endif
    applied = Double(to - from)
    // UIKit's deceleration: 0.998 a millisecond.
    let v = d.velocity * pow(0.998, dt * 1000)
    if abs(v) < 20 || to == from {
      drive = nil
      d.done.resume()
    } else {
      drive = (v, d.done)
    }
  }

  private static func enclosingScrollView(_ view: PlatformView) -> PlatformScrollView? {
    #if os(iOS)
      var v: UIView? = view.superview
      while let s = v {
        if let scroll = s as? UIScrollView { return scroll }
        v = s.superview
      }
      return nil
    #else
      return view.enclosingScrollView
    #endif
  }
}

private struct WeakView {
  weak var view: PlatformView?
}

/// The display link's target: a link retains it, not the probe.
private final class Tick: NSObject {
  weak var probe: FrameProbe?
  init(_ probe: FrameProbe) { self.probe = probe }

  @MainActor @objc func tick(_ link: CADisplayLink) { probe?.tick(link.timestamp) }
}

/// A row's marker: a view the size of the row, which the probe reads.
struct ProbeMarker: View {
  let id: String

  var body: some View {
    if FrameProbe.enabled { MarkerView(id: id) }
  }
}

#if os(iOS)
  private struct MarkerView: UIViewRepresentable {
    let id: String
    func makeUIView(context: Context) -> Marker { Marker(id) }
    func updateUIView(_ view: Marker, context: Context) {}
  }

  private final class Marker: UIView {
    let id: String
    init(_ id: String) {
      self.id = id
      super.init(frame: .zero)
      isUserInteractionEnabled = false
    }
    required init?(coder: NSCoder) { fatalError() }
    override func didMoveToWindow() {
      super.didMoveToWindow()
      if window != nil {
        FrameProbe.shared.register(id, self)
      } else {
        FrameProbe.shared.unregister(id, self)
      }
    }
  }
#else
  private struct MarkerView: NSViewRepresentable {
    let id: String
    func makeNSView(context: Context) -> Marker { Marker(id) }
    func updateNSView(_ view: Marker, context: Context) {}
  }

  private final class Marker: NSView {
    let id: String
    init(_ id: String) {
      self.id = id
      super.init(frame: .zero)
    }
    required init?(coder: NSCoder) { fatalError() }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
    override func viewDidMoveToWindow() {
      super.viewDidMoveToWindow()
      if window != nil {
        FrameProbe.shared.register(id, self)
      } else {
        FrameProbe.shared.unregister(id, self)
      }
    }
  }
#endif

/// The probe's summary, as an element the UI tests read.
struct ProbeSummary: View {
  @State private var probe = FrameProbe.shared

  var body: some View {
    if FrameProbe.enabled {
      Text(probe.summary.prefix(40))
        .font(.system(size: 8))
        .lineLimit(1)
        .accessibilityIdentifier("probe")
        .accessibilityValue(probe.summary)
    }
  }
}
