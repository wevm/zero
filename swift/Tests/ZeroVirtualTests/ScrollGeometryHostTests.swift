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
  private(set) var scrollRequests = 0
  private var reported: [String: (minY: Double, maxY: Double)] = [:]

  init(_ h: Harness, containerHeight: Double = 400, insetTop: Double = 0) {
    self.h = h
    self.containerHeight = containerHeight
    self.insetTop = insetTop
    // At rest at the top, the inset is above the content.
    offset = -insetTop
    host.requestScroll = { [unowned self] in
      requested = $0
      scrollRequests += 1
    }
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
    host.render(
      snapshot.items.map { (key: $0.key, index: $0.index) }, spaceBefore: snapshot.spaceBefore)
    // Only frames that changed are reported, as `onGeometryChange` does,
    // and a row's first: one that left and came back is a new view.
    let live = Set(snapshot.items.map(\.key))
    reported = reported.filter { live.contains($0.key) }
    var y = snapshot.spaceBefore
    for item in snapshot.items {
      let frame = (minY: y, maxY: y + height(item.key))
      if reported[item.key].map({ $0 != frame }) ?? true {
        reported[item.key] = frame
        host.rowFrame(item.key, minY: frame.minY, maxY: frame.maxY)
      }
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
      let before = (h.queryKey(h.core.queryInputs), offset, requested, snapshot.spaceBefore)
      pass()
      if (h.queryKey(h.core.queryInputs), offset, requested, h.core.snapshot.spaceBefore) == before,
        deferred.isEmpty,
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

  /// Where the row was drawn on the last pass, from the viewport's top.
  func drawnTop(_ key: String) -> Double? { reported[key].map { $0.minY - offset - insetTop } }

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

    host.render([(key: "a", index: 0), (key: "b", index: 1)], spaceBefore: 0)
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

    // A new space over the same first row moves the rows by its change,
    // with no new reports (the view reports a frame only when it changes;
    // here the update waits on the content reaching the moved rows).
    host.render([(key: "a", index: 0), (key: "b", index: 1)], spaceBefore: 30)
    run()
    #expect(updates == 1)
    #expect(host.rowFrames().map(\.maxY) == [50, 80])
    host.geometry(
      contentOffset: 0, containerHeight: 100, contentHeight: 80, insetTop: 0, insetBottom: 0)
    run()
    #expect(updates == 2)

    // A new first row: the space was sized to keep the rows in place, so
    // the ones still there keep their frames and only the new one waits.
    host.render([(key: "z", index: 0), (key: "a", index: 1), (key: "b", index: 2)], spaceBefore: 0)
    run()
    #expect(updates == 2)
    host.rowFrame("z", minY: 0, maxY: 30)
    run()
    #expect(updates == 3)
    #expect(host.rowFrames().map(\.maxY) == [30, 50, 80])

    // Content the view rounded to the pixel below the rows' fractional end.
    host.rowFrame("b", minY: 20, maxY: 60.33)
    host.geometry(
      contentOffset: 0, containerHeight: 100, contentHeight: 60, insetTop: 0, insetBottom: 0)
    run()
    #expect(updates == 4)
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

extension ScrollGeometryHostTests {
  @Test(
    "a fling up through pages of taller rows isn't scrolled under; at rest the space is given back")
  func flingKeepsGoing() {
    let h = Harness(rowCount: 400) { $0.anchoring = .manual }
    let view = GeometryScrollView(h)
    for i in 0..<400 where i % 3 == 0 { view.heights["r\(i)"] = 60 }
    view.settle()
    h.core.scrollToItem("r300", align: .start)
    view.settle()
    h.scheduler.advance(by: 200)
    view.settle()
    let before = view.scrollRequests

    // A fling: the offset moves every frame, and pages load under it.
    var expected = view.host.scrollOffset
    var key = h.core.firstVisibleItem()!.key
    for _ in 0..<60 {
      let top = view.rowTop(key)!
      view.userScroll(view.host.scrollOffset - 40)
      // Within the frame: the pages' rows, and the space that takes them up.
      view.settle()
      expected -= 40
      // Each frame moves what's on screen by the fling's 40 points, no more.
      #expect(view.rowTop(key) == top + 40)
      key = h.core.firstVisibleItem()!.key
    }
    #expect(view.scrollRequests == before)
    #expect(view.host.scrollOffset == max(0, expected))

    // At rest, the space goes back to its estimate, nothing on screen moving.
    let top = view.rowTop(key)!
    h.scheduler.advance(by: 200)
    view.settle()
    #expect(view.rowTop(key) == top)
  }
}

extension ScrollGeometryHostTests {
  @Test(
    "opened at its end, flings up to the top through rows 3× their estimate, never scrolled under")
  func flingsUpFromAPermalink() {
    let h = Harness(rowCount: 400) {
      $0.anchoring = .manual
      $0.permalinkID = "r399"
    }
    let view = GeometryScrollView(h)
    for i in 0..<400 { view.heights["r\(i)"] = 60 }
    view.settle()
    h.scheduler.advance(by: 200)
    view.settle()
    #expect(h.core.lastVisibleItem()?.key == "r399")

    var flings = 0
    while view.host.scrollOffset > 0, flings < 200 {
      flings += 1
      let before = view.scrollRequests
      // A fling of 8 coarse frames, pages loading under it. (Kept cheap: it
      // runs on the main actor alongside the other targets' tests.)
      for _ in 0..<8 where view.host.scrollOffset > 0 {
        guard let key = h.core.firstVisibleItem()?.key, let top = view.rowTop(key) else {
          Issue.record("no rows on screen")
          return
        }
        let offset = view.host.scrollOffset
        view.userScroll(offset - 100)
        view.settle()
        let moved = offset - max(0, offset - 100)
        #expect(view.rowTop(key) == top + moved, "fling \(flings) at \(offset)")
      }
      #expect(view.scrollRequests == before, "scrolled under fling \(flings)")
      // At rest: the space is given back, nothing on screen moving.
      guard let key = h.core.firstVisibleItem()?.key, let top = view.rowTop(key) else { return }
      let (space, first) = (h.core.snapshot.spaceBefore, h.core.snapshot.items.first?.key)
      h.scheduler.advance(by: 200)
      // Space taken out goes with its scroll: no pass drawn at the old offset.
      view.pass()
      if h.core.snapshot.spaceBefore < space, h.core.snapshot.items.first?.key == first {
        #expect(view.drawnTop(key) == top, "the give-back after fling \(flings) drew rows moved")
      }
      view.settle()
      #expect(view.rowTop(key) == top, "the give-back after fling \(flings) moved the rows")
    }
    #expect(h.core.snapshot.items.first?.key == "r0")
    #expect(view.host.scrollOffset == 0)
    #expect(h.core.snapshot.spaceBefore == 0)
  }

  @Test("a scroll a report contradicts is asked for again, a few times")
  func reissuesAContradictedScroll() {
    let host = ScrollGeometryHost()
    var requested: [Double] = []
    var scrolls = 0
    host.requestScroll = { requested.append($0) }
    host.didScroll = { scrolls += 1 }
    func report(_ offset: Double, content: Double = 2000) {
      host.geometry(
        contentOffset: offset, containerHeight: 400, contentHeight: content, insetTop: 0,
        insetBottom: 0)
    }
    report(500)
    scrolls = 0
    host.scroll(to: 1200)
    // A fling's deceleration writes over it: asked for again, the core not
    // told of an offset it didn't ask for.
    report(490)
    #expect(requested == [1200, 1200])
    #expect(host.scrollOffset == 1200)
    #expect(scrolls == 0)
    // Against the content as it is now.
    report(480, content: 1500)
    #expect(requested == [1200, 1200, 1100])
    report(1100, content: 1500)
    #expect(scrolls == 1)
    #expect(host.scrollOffset == 1100)

    // One that never takes is given up after three reports.
    host.scroll(to: 200)
    for y in [1090.0, 1080, 1070, 1060] { report(y, content: 1500) }
    #expect(requested.count == 3 + 1 + 3)
    #expect(host.scrollOffset == 1060)
  }
}
