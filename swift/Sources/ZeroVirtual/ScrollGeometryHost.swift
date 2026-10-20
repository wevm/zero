/// A `VirtualHost` for a scroll view that reports its geometry after the
/// fact, as SwiftUI's does: its offset with `onScrollGeometryChange`, and
/// each row's frame with `onGeometryChange`.
///
/// It keeps what the core asks of a scroll view as of the last report: row
/// frames in content coordinates, by key; the offset from the content's top,
/// insets aside. A scroll the core asks for takes effect here at once, and
/// goes to the view with `requestScroll`. The core hears of a new snapshot
/// (`afterUpdate`) once every one of its rows has been measured.
///
/// The snapshot is already the window to draw, so the view lays its rows
/// out eagerly (`VStack`, not `LazyVStack`): a lazy stack might never
/// measure a row out of sight, and the update would wait for it forever.
/// Frames are in the rows' stack's coordinate space, which doesn't move as
/// the view scrolls.
@MainActor
package final class ScrollGeometryHost: VirtualHost {
  package private(set) var scrollOffset = 0.0
  package private(set) var viewportHeight = 0.0
  package private(set) var contentHeight = 0.0
  package var viewportInsets: (top: Double, bottom: Double) { (0, 0) }

  /// Scrolls the view so content `y` is at the viewport's top.
  package var requestScroll: (Double) -> Void = { _ in }
  /// Runs `action` once the view has reported a layout pass: later on the
  /// main actor.
  package var defer_: (@escaping @MainActor () -> Void) -> Void = { action in
    Task { @MainActor in action() }
  }
  package var didScroll: () -> Void = {}
  package var viewportDidResize: () -> Void = {}
  package var afterUpdate: () -> Void = {}

  /// The rendered rows, in list order.
  private var items: [(key: AnyHashable, index: Int)] = []
  private var spaceBefore = 0.0
  private var heights: [AnyHashable: (minY: Double, maxY: Double)] = [:]
  private var pendingScroll: Double?
  private var flushScheduled = false

  package init() {}

  package func rowFrames() -> [VirtualRowFrame] {
    items.compactMap { item in
      heights[item.key].map {
        VirtualRowFrame(index: item.index, key: item.key, minY: $0.minY, maxY: $0.maxY)
      }
    }
  }

  package func scroll(to target: Double) {
    let offset = max(0, min(target, contentHeight - viewportHeight))
    guard offset != scrollOffset else { return }
    scrollOffset = offset
    pendingScroll = offset
    requestScroll(offset)
  }

  /// The view rendered a snapshot with these rows: every snapshot, even one
  /// with the same rows, since the core waits on `afterUpdate` for it.
  package func render<Key: Hashable>(_ keys: [(key: Key, index: Int)], spaceBefore: Double) {
    items = keys.map { (AnyHashable($0.key), $0.index) }
    let live = Set(items.map(\.key))
    // A new space before the rows moves every row: frames from before are
    // stale, and the core would take the move it asked for as a new one.
    if spaceBefore != self.spaceBefore {
      self.spaceBefore = spaceBefore
      heights = [:]
    } else {
      heights = heights.filter { live.contains($0.key) }
    }
    scheduleFlush()
  }

  /// A row's frame, in content coordinates.
  package func rowFrame<Key: Hashable>(_ key: Key, minY: Double, maxY: Double) {
    let key = AnyHashable(key)
    if let old = heights[key], old == (minY, maxY) { return }
    heights[key] = (minY, maxY)
    scheduleFlush()
  }

  /// The scroll view's geometry: `contentOffset` as UIKit reports it (the
  /// top inset is negative space above the content), its full height,
  /// insets included (UIKit's bounds), and its insets.
  package func geometry(
    contentOffset: Double, containerHeight: Double, contentHeight: Double, insetTop: Double,
    insetBottom: Double
  ) {
    let viewport = max(0, containerHeight - insetTop - insetBottom)
    let resized = viewport != viewportHeight
    let grew = contentHeight != self.contentHeight
    viewportHeight = viewport
    self.contentHeight = contentHeight
    let offset = contentOffset + insetTop
    let moved = offset != scrollOffset
    let landed = pendingScroll != nil
    pendingScroll = nil
    scrollOffset = offset
    if resized { viewportDidResize() }
    // The core's own scroll is reported too, landed or not: that's how it
    // tells its scrolls from the user's.
    if moved || landed { didScroll() }
    if grew { scheduleFlush() }
  }

  private func scheduleFlush() {
    guard !flushScheduled else { return }
    flushScheduled = true
    defer_ { [weak self] in self?.flush() }
  }

  private func flush() {
    flushScheduled = false
    // A row not measured yet is laid out on a pass still coming; so is
    // content that doesn't reach the last row yet. Rows can report before
    // the scroll view does, and the core scrolls within `contentHeight`.
    // A point of slack: the scroll view rounds its content size to pixels,
    // and rows of text have fractional heights.
    guard items.allSatisfy({ heights[$0.key] != nil }),
      contentHeight + 1 >= (rowFrames().last?.maxY ?? 0)
    else { return }
    afterUpdate()
  }
}
