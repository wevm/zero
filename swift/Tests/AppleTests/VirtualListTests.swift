#if canImport(SwiftUI)
  import Foundation
  import SwiftUI
  import Testing
  import ZeroTesting

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

    func makeList() throws -> (Zero<Void>, List) {
      let server = LoopbackServer()
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

    @Test func scrollsToARowFarBelow() async throws {
      let (zero, list) = try makeList()
      let window = show(VirtualList(list) { Text($0.row.text).frame(height: 30) })
      try await run(5) { list.snapshot.complete && !list.snapshot.items.isEmpty }
      #expect(list.snapshot.items.first?.key == Self.id(0))
      list.scrollToItem(Self.id(250), align: .end)
      try await run(5) { list.lastVisibleItem()?.key == Self.id(250) }
      try await run(0.5) { false }
      #expect(list.lastVisibleItem()?.key == Self.id(250))
      // The scroll view itself moved there, not just the virtualizer's idea
      // of it: n250's bottom at the viewport's. Its rows are 30 points, after
      // the space that stands in for the rows before the window.
      let g = try #require(platformGeometry(window))
      let s = list.snapshot
      let first = try #require(s.items.first?.index)
      #expect(abs(g.offset - (s.spaceBefore + Double(250 - first + 1) * 30 - g.viewport)) <= 1)
      withExtendedLifetime((zero, window)) {}
    }
  }
#endif
