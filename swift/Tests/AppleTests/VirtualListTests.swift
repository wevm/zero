#if canImport(SwiftUI)
  import Foundation
  import SwiftUI
  import Testing
  import ZeroTesting
  import ZeroUI

  @testable import Zero
  @testable import ZeroVirtual

  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif

  /// `VirtualList` in a real window, on a `LoopbackServer`.
  @Suite(.serialized) @MainActor struct VirtualListTests {
    struct Page: Codable, Sendable {
      var limit: Int
      var start: String?
      var backward: Bool
    }

    static let page = defineQuery("notes.page") { (a: Page) in
      var q = Notes.table.orderBy(\.id, a.backward ? .desc : .asc)
      if let s = a.start { q = q.start([\.id: s]) }
      return q.limit(a.limit)
    }
    static let byID = defineQuery("notes.byID") { (id: String) in
      Notes.table.where(\.id, id).one()
    }

    typealias List = ZeroVirtualizer<Void, Note, String, String, String>

    static func id(_ i: Int) -> String { String(format: "n%03d", i) }

    func makeList(server: LoopbackServer = LoopbackServer()) throws -> (Zero<Void>, List) {
      server.put(
        "note", (0..<300).map { ["id": .string(Self.id($0)), "text": .string("note \($0)")] })
      var options = ZeroOptions(
        cacheURL: "http://loopback.test", userID: "u", storageKey: randomID(), storage: .memory)
      options.logSink = RecordingLogSink()
      let zero = try Zero(schema: .notes, options: options, socketFactory: server)
      let list = List(
        zero: zero, listContext: "", estimatedRowHeight: 30, rowKey: \.id, toStartRow: \.id,
        pageQuery: { r, _ in
          Self.page(Page(limit: r.limit, start: r.start, backward: r.direction == .backward))
        },
        rowQuery: { r, _ in Self.byID(r.id) }, minPageSize: 20)
      return (zero, list)
    }

    /// A 300 × 400 window showing `view`.
    func show(_ view: some View) -> AnyObject {
      #if canImport(UIKit)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 300, height: 400))
        window.rootViewController = UIHostingController(rootView: view)
        window.makeKeyAndVisible()
        return window
      #else
        let window = NSWindow(
          contentRect: CGRect(x: 0, y: 0, width: 300, height: 400), styleMask: [.titled],
          backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: view)
        window.makeKeyAndOrderFront(nil)
        return window
      #endif
    }

    /// Waits until `done`, for at most `seconds`, leaving the main actor
    /// free: the server's deliveries and SwiftUI's layout run on it. (A
    /// nested run loop wouldn't drain the main queue from a test running on
    /// it.)
    func run(_ seconds: Double, until done: () -> Bool) async throws {
      let end = Date().addingTimeInterval(seconds)
      while Date() < end, !done() { try await Task.sleep(for: .milliseconds(20)) }
    }

    /// The scroll view's offset from the content's top and the height it
    /// shows content in, insets aside, as the platform has them.
    func platformGeometry(_ window: AnyObject) -> (offset: Double, viewport: Double)? {
      #if canImport(UIKit)
        func find(_ v: UIView) -> UIScrollView? {
          (v as? UIScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        return (window as? UIWindow).flatMap(find).map {
          let inset = $0.adjustedContentInset
          return (
            Double($0.contentOffset.y + inset.top),
            Double($0.bounds.height - inset.top - inset.bottom)
          )
        }
      #else
        func find(_ v: NSView) -> NSScrollView? {
          (v as? NSScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        return (window as? NSWindow)?.contentView.flatMap(find).map {
          let inset = $0.contentInsets
          return (
            Double($0.contentView.bounds.origin.y + inset.top),
            Double($0.contentView.bounds.height - inset.top - inset.bottom)
          )
        }
      #endif
    }

    final class Seen {
      var texts: Set<String> = []
    }

    /// A row that runs its own query.
    struct QueryRow: View {
      @ZeroQuery<Note, Note?, Void> var note: Note?
      let seen: Seen

      init(id: String, seen: Seen) {
        _note = ZeroQuery(VirtualListTests.byID(id))
        self.seen = seen
      }

      var body: some View {
        let _ = note.map { seen.texts.insert($0.text) }
        Text(note?.text ?? "").frame(height: 30)
      }
    }

    // Rows are hosted views of their own, which inherit the list's environment.
    @Test func aRowsQueryReadsTheListsClient() async throws {
      let (zero, list) = try makeList()
      let seen = Seen()
      let window = show(VirtualList(list) { QueryRow(id: $0.row.id, seen: seen) }.zero(zero))
      try await run(30) { seen.texts.contains("note 0") }
      #expect(seen.texts.contains("note 0"))
      withExtendedLifetime((zero, window)) {}
    }

    @Observable final class Selection {
      var key: String?
      /// Each key a tap or a key set it to.
      var sets: [String] = []
      /// Whether each row was last shown selected.
      @ObservationIgnored var shown: [String: Bool] = [:]

      var binding: Binding<String?> {
        Binding {
          self.key
        } set: {
          self.key = $0
          if let key = $0 { self.sets.append(key) }
        }
      }
    }

    @Test func selectsByTapAndByKeys() async throws {
      let (zero, list) = try makeList()
      let selection = Selection()
      let window = show(
        VirtualList(list, selection: selection.binding) { item, isSelected in
          let _ = selection.shown[item.row.id] = isSelected
          Text(item.row.text).frame(height: 30)
        })
      try await run(30) { list.snapshot.complete && !list.snapshot.items.isEmpty }
      let view = try #require(rowsScrollView(window))
      try await RowsSelectionCheck(list: list, view: view, window: window) {
        (selection.key, selection.sets, selection.shown)
      }.run(self)
      withExtendedLifetime((zero, window)) {}
    }

    // A snapshot after a tap draws the list again: the selection is the
    // binding's now, not what it was when the list was made.
    @Test func keepsTheSelectionThroughASnapshot() async throws {
      let server = LoopbackServer()
      let (zero, list) = try makeList(server: server)
      let selection = Selection()
      let window = show(
        VirtualList(list, selection: selection.binding) { item, _ in
          Text(item.row.text).frame(height: 30)
        })
      try await run(30) { list.snapshot.complete && !list.snapshot.items.isEmpty }
      let view = try #require(rowsScrollView(window))
      let check = RowsSelectionCheck(list: list, view: view, window: window) {
        (selection.key, selection.sets, selection.shown)
      }
      try await run(5) { view.rows.host.rowFrames().count > 5 }
      try check.tap(Self.id(7))
      try await run(5) { check.highlighted(Self.id(7)) }
      server.put("note", ["id": .string(Self.id(2)), "text": .string("changed")])
      try await run(30) { list.snapshot.items.contains { $0.row.text == "changed" } }
      try await run(0.5) { false }
      #expect(selection.key == Self.id(7))
      try check.expectHighlight(on: Self.id(7))
      withExtendedLifetime((zero, window)) {}
    }

    /// The `RowsScrollView` in `window`.
    func rowsScrollView(_ window: AnyObject) -> RowsScrollView? {
      #if canImport(UIKit)
        func find(_ v: UIView) -> RowsScrollView? {
          (v as? RowsScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        return (window as? UIWindow).flatMap(find)
      #else
        func find(_ v: NSView) -> RowsScrollView? {
          (v as? RowsScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        return (window as? NSWindow)?.contentView.flatMap(find)
      #endif
    }

    /// A row with state of its own: set on the first row only.
    struct MarkedRow: View {
      let id: String
      let seen: Seen
      @State private var marked = false

      var body: some View {
        let _ = marked ? seen.texts.insert(id) : nil
        Text(id).frame(height: 30).onAppear { if id == VirtualListTests.id(0) { marked = true } }
      }
    }

    // A row's view is reused for the rows that come, as a `List`'s are; its
    // state stays with its row.
    @Test func aRowsStateDoesNotGoToTheRowReusingItsView() async throws {
      let (zero, list) = try makeList()
      let seen = Seen()
      let window = show(VirtualList(list) { MarkedRow(id: $0.row.id, seen: seen) })
      try await run(30) { seen.texts.contains(Self.id(0)) }
      for i in stride(from: 20, through: 120, by: 20) {
        list.scrollToItem(Self.id(i), align: .end)
        try await run(30) { list.lastVisibleItem()?.key == Self.id(i) }
      }
      try await run(0.5) { false }
      #expect(seen.texts == [Self.id(0)])
      withExtendedLifetime((zero, window)) {}
    }

    final class States {
      var byRow: [String: Set<UUID>] = [:]
    }

    /// A row that records the state it's drawn with: one whose view starts
    /// anew has a new one.
    struct StatefulRow: View {
      let id: String
      let states: States
      @State private var state = UUID()

      var body: some View {
        let _ = states.byRow[id, default: []].insert(state)
        Text(id).frame(height: 30)
      }
    }

    @Test func aKeptRowComesBackWithItsState() async throws {
      let (zero, list) = try makeList()
      let states = States()
      let window = show(
        VirtualList(list) { StatefulRow(id: $0.row.id, states: states) }.maxKeptRows(300))
      let top = (0..<10).map(Self.id)
      try await run(30) { top.allSatisfy { states.byRow[$0] != nil } }
      list.scrollToItem(Self.id(250), align: .end)
      try await run(30) { list.lastVisibleItem()?.key == Self.id(250) }
      try await run(0.5) { false }
      #expect(!list.snapshot.items.contains { $0.key == Self.id(0) })
      list.scrollToItem(Self.id(0), align: .start)
      try await run(30) { list.firstVisibleItem()?.key == Self.id(0) }
      try await run(0.5) { false }
      #expect(top.map { states.byRow[$0]?.count } == top.map { _ in 1 })
      withExtendedLifetime((zero, window)) {}
    }

    /// Row views that record what the layout asks of them: plain views,
    /// 30 points high.
    @MainActor final class CountingRowViews: RowViews {
      /// Each row shown in a view, and its state.
      var shown: [(key: AnyHashable, state: RowState)] = []
      var measured = 0

      func view(
        for item: RowsItem, _ state: RowState, reusing view: PlatformView?, in layout: RowsLayout
      ) -> PlatformView {
        shown.append((item.key, state))
        return view ?? PlatformView(frame: .zero)
      }

      func height(of view: PlatformView, width: Double) -> Double {
        measured += 1
        return 30
      }

      func discard(_ view: PlatformView) {}
    }

    /// A layout of 30-point rows, with no frames after its passes: what a
    /// pass leaves for the next frame doesn't run.
    func countingLayout(keeping maxKeptRows: Int) -> (RowsLayout, CountingRowViews) {
      let views = CountingRowViews()
      let rows = RowsLayout(views: views)
      rows.host.estimatedRowHeight = { 30 }
      rows.nextFrame = { _ in }
      rows.maxKeptRows = maxKeptRows
      return (rows, views)
    }

    /// A pass with rows `range` scrolled to at `width`, with `changed`
    /// rows' data changed: the rows it showed in a view.
    @discardableResult
    func pass(
      _ range: Range<Int>, in rows: RowsLayout, _ views: CountingRowViews, width: Double = 300,
      changed: Set<Int> = []
    ) -> [AnyHashable] {
      views.shown = []
      views.measured = 0
      rows.update(
        RowsSource(
          items: range.map {
            RowsItem(
              key: AnyHashable(Self.id($0)), index: $0, row: changed.contains($0) ? -1 : $0,
              item: $0)
          }, spaceBefore: Double(range.lowerBound) * 30))
      _ = rows.layout(width: width, offset: Double(range.lowerBound) * 30, viewport: 400)
      return views.shown.map(\.key)
    }

    // Rows that page out keep their views, up to `maxKeptRows`, and come
    // back in them, neither shown nor measured again; 0 keeps none.
    @Test(arguments: [0, 5, 100])
    func keepsTheViewsOfRowsThatLeave(maxKeptRows: Int) {
      let (rows, views) = countingLayout(keeping: maxKeptRows)
      let first = pass(0..<20, in: rows, views)
      pass(100..<120, in: rows, views)
      let back = pass(0..<20, in: rows, views)
      let anew = first.count - min(maxKeptRows, first.count)
      #expect(first.filter(back.contains).count == anew)
      #expect(views.measured == anew)
    }

    // A kept row comes back as it is now: with its new data, in its
    // selection, and measured at the list's width, all changed while it was
    // kept.
    @Test func aKeptRowComesBackAsItIsNow() throws {
      let (rows, views) = countingLayout(keeping: 100)
      let first = pass(0..<20, in: rows, views)
      pass(100..<120, in: rows, views)
      rows.selected = AnyHashable(Self.id(3))
      pass(100..<120, in: rows, views, width: 250)
      let back = pass(0..<20, in: rows, views, width: 250, changed: [1])
      #expect(Set(back) == [AnyHashable(Self.id(1)), AnyHashable(Self.id(3))])
      let selected = try #require(views.shown.first { $0.key == AnyHashable(Self.id(3)) })
      #expect(selected.state.selected)
      #expect(views.measured == first.count)
    }

    @Test func scrollsToARowFarBelow() async throws {
      let (zero, list) = try makeList()
      let window = show(VirtualList(list) { Text($0.row.text).frame(height: 30) })
      // Generous: other tests in the process can hold the main actor for seconds.
      try await run(30) { list.snapshot.complete && !list.snapshot.items.isEmpty }
      #expect(list.snapshot.items.first?.key == Self.id(0))
      list.scrollToItem(Self.id(250), align: .end)
      try await run(30) { list.lastVisibleItem()?.key == Self.id(250) }
      try await run(0.5) { false }
      #expect(list.lastVisibleItem()?.key == Self.id(250))
      // The scroll view itself moved there, not just the virtualizer's idea
      // of it: n250's bottom at the viewport's. Its rows are 30 points, after
      // the space that stands in for the rows before the window.
      let g = try #require(platformGeometry(window))
      let s = list.snapshot
      // Its place in the window: under a permalink anchor, `index` counts
      // from the window's anchor, not the list's start.
      let position = try #require(s.items.firstIndex { $0.key == Self.id(250) })
      #expect(abs(g.offset - (s.spaceBefore + Double(position + 1) * 30 - g.viewport)) <= 1)
      withExtendedLifetime((zero, window)) {}
    }
  }

  /// Selecting rows of 30 points, ids `n000`…, in a 400-point list: by a
  /// tap, twice on the same row, then on macOS by the arrow keys and j and
  /// k (sent through the window), past the screen's bottom. iOS has no
  /// keys for it: there, a tap neither takes first responder nor offers
  /// key commands.
  @MainActor struct RowsSelectionCheck {
    let list: VirtualListTests.List
    let view: RowsScrollView
    let window: AnyObject
    /// The selection, every key the user set it to, and whether each row
    /// was last shown selected.
    let state: () -> (key: String?, sets: [String], shown: [String: Bool])

    /// The row selected last, and how many times the user selected one:
    /// three taps on iOS; two taps and thirteen keys on macOS.
    #if canImport(UIKit)
      static let last = VirtualListTests.id(7)
      static let selections = 3
    #else
      static let last = VirtualListTests.id(14)
      static let selections = 15
    #endif

    func run(_ t: VirtualListTests) async throws {
      let id = VirtualListTests.id
      // Laid out: the snapshot can complete before the view's next pass.
      try await t.run(5) { view.rows.host.rowFrames().count > 5 }
      try tap(id(5))
      try await t.run(5) { state().key == id(5) }
      #expect(state().key == id(5))
      #expect(state().shown[id(5)] == true)
      try await t.run(5) { highlighted(id(5)) }
      try expectHighlight(on: id(5))

      // Again: the app hears of it, to open the row again.
      try tap(id(5))
      try await t.run(5) { state().sets.count == 2 }
      #expect(state().sets == [id(5), id(5)])

      #if canImport(UIKit)
        #expect(!view.isFirstResponder)
        #expect(!view.canBecomeFirstResponder)
        #expect(view.keyCommands?.isEmpty ?? true)
        // Another row, by a tap.
        try tap(id(7))
        try await t.run(5) { state().key == id(7) }
        #expect(state().shown[id(5)] == false)
        #expect(state().shown[id(7)] == true)
        try await t.run(5) { highlighted(id(7)) }
        try expectHighlight(on: id(7))
      #else
        try await keys(t)
      #endif
    }

    #if canImport(AppKit)
      /// The arrow keys and j and k, from the tapped row.
      func keys(_ t: VirtualListTests) async throws {
        let id = VirtualListTests.id
        for _ in 0..<10 { key(down: true, letter: false) }
        try await t.run(5) { state().key == id(15) }
        #expect(state().key == id(15))
        // Below the screen at first: scrolled into view, its bottom at the
        // list's.
        try await t.run(5) { list.lastVisibleItem()?.key == id(15) }
        #expect(list.lastVisibleItem()?.key == id(15))
        key(down: false, letter: true)
        key(down: false, letter: true)
        key(down: true, letter: true)
        try await t.run(5) { state().sets.count == 15 }
        #expect(state().key == id(14))
        // The keys run without a layout pass between them: a row they
        // scrolled into view gets its view in the pass that places the
        // highlight, unless the frames before made it off screen.
        try await t.run(5) { highlighted(id(14)) }
        #expect(state().shown[id(15)] == false)
        #expect(state().shown[id(5)] == false)
        #expect(state().shown[id(14)] == true)
        try expectHighlight(on: id(14))
      }

      /// The down or up arrow, or j or k.
      func key(down: Bool, letter: Bool) {
        let w = window as! NSWindow
        let (characters, code): (String, UInt16) =
          letter
          ? (down ? ("j", 38) : ("k", 40))
          : (
            String(UnicodeScalar(down ? NSDownArrowFunctionKey : NSUpArrowFunctionKey)!),
            down ? 125 : 126
          )
        w.sendEvent(
          NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: letter ? [] : [.numericPad, .function],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: w.windowNumber,
            context: nil, characters: characters, charactersIgnoringModifiers: characters,
            isARepeat: false, keyCode: code)!)
      }
    #endif

    func frame(_ key: String) throws -> VirtualRowFrame {
      try #require(view.rows.host.rowFrames().first { $0.key == AnyHashable(key) })
    }

    /// A tap's own handler, at the row's middle: UIKit has no way to send
    /// a touch, nor AppKit a click to a window that isn't key, as a test's
    /// isn't.
    func tap(_ key: String) throws {
      let f = try frame(key)
      view.select(at: (f.minY + f.maxY) / 2)
    }

    /// The highlight is shown behind the row's frame. It's placed in the
    /// view's next layout pass, which a slow runner can take a while to run.
    func highlighted(_ key: String) -> Bool {
      guard let f = try? frame(key) else { return false }
      let h = view.highlight
      return !h.isHidden && abs(Double(h.frame.minY) - f.minY) <= 0.5
        && abs(Double(h.frame.maxY) - f.maxY) <= 0.5
    }

    /// The highlight stands behind the row's frame.
    func expectHighlight(on key: String) throws {
      let f = try frame(key)
      let h = view.highlight
      #expect(!h.isHidden)
      #expect(
        abs(Double(h.frame.minY) - f.minY) <= 0.5 && abs(Double(h.frame.maxY) - f.maxY) <= 0.5)
    }
  }
#endif
