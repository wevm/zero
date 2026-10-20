import Testing

@testable import ZeroVirtual

/// A SwiftUI scroll view as `ScrollGeometryHost` sees it: geometry reported
/// after each layout pass, row frames before the scroll view's own, a
/// requested scroll applied on the next pass, and deferred work run between
/// passes.
@MainActor
final class GeometryScrollView {
  let host = ScrollGeometryHost()
  let h: Harness
  var containerHeight: Double
  let insetTop: Double
  var heights: [String: Double] = [:]
  private var offset = 0.0
  private var requested: Double?
  private var deferred: [@MainActor () -> Void] = []
  private var snapshot = VirtualSnapshot<TestRow, String>()
  private(set) var afterUpdates = 0

  init(_ h: Harness, containerHeight: Double = 400, insetTop: Double = 0) {
    self.h = h
    self.containerHeight = containerHeight
    self.insetTop = insetTop
    // At rest at the top, the inset is above the content.
    offset = -insetTop
    host.requestScroll = { [unowned self] in requested = $0 }
    host.defer_ = { [unowned self] in deferred.append($0) }
    host.didScroll = { [unowned self] in self.h.core.didScroll() }
    host.viewportDidResize = { [unowned self] in self.h.core.viewportDidResize() }
    host.afterUpdate = { [unowned self] in
      afterUpdates += 1
      self.h.core.afterUpdate()
    }
    h.core.attach(host)
  }

  private func height(_ key: String) -> Double { heights[key] ?? h.view.rowHeight }

  private var contentHeight: Double {
    snapshot.spaceBefore + snapshot.items.reduce(0) { $0 + height($1.key) } + snapshot.spaceAfter
  }

  /// One body evaluation and layout pass, then the work it deferred.
  func pass() {
    h.core.setRows(h.answer(h.core.queryInputs))
    // Every snapshot is rendered, as `ZeroVirtualizer.onSnapshot` has it.
    snapshot = h.core.snapshot
    host.render(snapshot.items.map { (key: $0.key, index: $0.index) })
    var y = snapshot.spaceBefore
    for item in snapshot.items {
      host.rowFrame(item.key, minY: y, maxY: y + height(item.key))
      y += height(item.key)
    }
    let maxOffset = max(0, contentHeight - (containerHeight - insetTop)) - insetTop
    if let target = requested {
      requested = nil
      offset = target - insetTop
    }
    offset = max(-insetTop, min(offset, maxOffset))
    host.geometry(
      contentOffset: offset, containerHeight: containerHeight, contentHeight: contentHeight,
      insetTop: insetTop, insetBottom: 0)
    let work = deferred
    deferred = []
    work.forEach { $0() }
  }

  func settle(sourceLocation: SourceLocation = #_sourceLocation) {
    for _ in 0..<30 {
      let before = (h.queryKey(h.core.queryInputs), offset, requested)
      pass()
      if (h.queryKey(h.core.queryInputs), offset, requested) == before, deferred.isEmpty,
        h.core.snapshot.complete
      {
        return
      }
    }
    Issue.record("did not settle", sourceLocation: sourceLocation)
  }

  func userScroll(_ y: Double) {
    offset = y - insetTop
    pass()
  }

  /// Where the row is from the viewport's top.
  func rowTop(_ key: String) -> Double? {
    host.rowFrames().first { $0.key == AnyHashable(key) }.map { $0.minY - host.scrollOffset }
  }

  var bottomGap: Double { host.contentHeight - host.scrollOffset - host.viewportHeight }
}

@MainActor
@Suite("SwiftUI geometry host") struct ScrollGeometryHostTests {
  @Test("the update waits for every row to be measured and the content to reach them")
  func waitsForMeasurement() {
    let host = ScrollGeometryHost()
    var deferred: [@MainActor () -> Void] = []
    var updates = 0
    host.defer_ = { deferred.append($0) }
    host.afterUpdate = { updates += 1 }
    func run() {
      let work = deferred
      deferred = []
      work.forEach { $0() }
    }

    host.render([(key: "a", index: 0), (key: "b", index: 1)])
    host.rowFrame("a", minY: 0, maxY: 20)
    run()
    #expect(updates == 0)
    host.rowFrame("b", minY: 20, maxY: 50)
    run()
    // The rows are in, the scroll view hasn't reported content that fits them.
    #expect(updates == 0)
    host.geometry(
      contentOffset: 0, containerHeight: 100, contentHeight: 50, insetTop: 0, insetBottom: 0)
    run()
    #expect(updates == 1)
    #expect(host.rowFrames().map(\.maxY) == [20, 50])
  }

  @Test("its own scroll lands and is reported; the user's is reported once")
  func reportsScrolls() {
    let host = ScrollGeometryHost()
    var requested: [Double] = []
    var scrolls = 0
    host.requestScroll = { requested.append($0) }
    host.didScroll = { scrolls += 1 }
    host.geometry(
      contentOffset: -10, containerHeight: 110, contentHeight: 1000, insetTop: 10, insetBottom: 0)
    #expect(host.scrollOffset == 0)
    #expect(host.viewportHeight == 100)
    #expect(scrolls == 0)

    host.scroll(to: 2000)
    #expect(requested == [900])
    #expect(host.scrollOffset == 900)
    // Landing where it already says it is still tells the core.
    host.geometry(
      contentOffset: 890, containerHeight: 110, contentHeight: 1000, insetTop: 10, insetBottom: 0)
    #expect(scrolls == 1)
    host.geometry(
      contentOffset: 890, containerHeight: 110, contentHeight: 1000, insetTop: 10, insetBottom: 0)
    #expect(scrolls == 1)
    host.geometry(
      contentOffset: 490, containerHeight: 110, contentHeight: 1000, insetTop: 10, insetBottom: 0)
    #expect(scrolls == 2)
    #expect(host.scrollOffset == 500)
  }

  @Test("pages down and back up, the top row holding still as pages come and go")
  func pages() {
    let h = Harness(rowCount: 1000) { $0.anchoring = .manual }
    let view = GeometryScrollView(h, containerHeight: 430, insetTop: 30)
    view.settle()
    #expect(h.core.snapshot.items.first?.key == "r0")

    for _ in 0..<40 {
      view.userScroll(view.host.scrollOffset + 300)
      view.settle()
    }
    let key = "r600"
    #expect(view.rowTop(key) != nil)
    let top = view.rowTop(key)
    view.settle()
    #expect(view.rowTop(key) == top)

    for _ in 0..<40 {
      view.userScroll(view.host.scrollOffset - 300)
      view.settle()
    }
    #expect(h.core.snapshot.items.first?.key == "r0")
    #expect(view.host.scrollOffset == 0)
  }

  @Test("rows taller than estimated above the viewport don't move it")
  func dynamicHeights() {
    let h = Harness(rowCount: 300) { $0.anchoring = .manual }
    let view = GeometryScrollView(h)
    view.settle()
    view.userScroll(2000)
    view.settle()
    let key = h.core.firstVisibleItem()!.key
    let top = view.rowTop(key)!
    let index = h.core.firstVisibleItem()!.index
    // Rows above it, rendered as overscan, turn out taller.
    for i in max(0, index - 4)..<index { view.heights["r\(i)"] = 75 }
    view.settle()
    #expect(view.rowTop(key) == top)
  }

  @Test("stuck to the bottom, it follows new rows and a last row growing taller")
  func sticks() {
    let h = Harness(rowCount: 60) {
      $0.anchoring = .manual
      $0.stickToBottom = true
    }
    let view = GeometryScrollView(h)
    view.settle()
    view.userScroll(.infinity)
    view.settle()
    #expect(view.bottomGap == 0)

    h.data += (60..<63).map { TestRow(id: "r\($0)") }
    view.settle()
    #expect(view.bottomGap == 0)
    #expect(h.core.lastVisibleItem()?.key == "r62")

    view.heights["r62"] = 160
    view.settle()
    #expect(view.bottomGap == 0)

    view.userScroll(view.host.scrollOffset - 300)
    let offset = view.host.scrollOffset
    h.data += (63..<66).map { TestRow(id: "r\($0)") }
    view.settle()
    #expect(view.host.scrollOffset == offset)
  }
}

extension ScrollGeometryHostTests {
  @Test("scrolls to a row not loaded yet, under a top inset")
  func scrollToItemFarBelow() {
    let h = Harness(rowCount: 200) { $0.anchoring = .manual }
    let view = GeometryScrollView(h, containerHeight: 459, insetTop: 59)
    view.settle()
    h.core.scrollToItem("r199", align: .end)
    view.settle()
    #expect(h.core.lastVisibleItem()?.key == "r199")
    #expect(view.bottomGap == 0)
  }
}
