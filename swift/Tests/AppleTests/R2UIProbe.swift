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

  /// Scratch probes for the r2 hardening round; not to be merged.
  @Suite(.serialized) @MainActor struct R2UIProbe {
    /// Rows that take any height they're offered: what does the list measure them at?
    @Test func flexibleRowHeight() async throws {
      let t = VirtualListTests()
      let (zero, list) = try t.makeList()
      let window = t.show(VirtualList(list) { _ in Color.blue.frame(maxHeight: .infinity) })
      try await t.run(10) { list.snapshot.complete && !list.snapshot.items.isEmpty }
      try await t.run(1) { false }
      var content = -1.0
      #if canImport(UIKit)
        func find(_ v: UIView) -> UIScrollView? {
          (v as? UIScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        if let s = (window as? UIWindow).flatMap(find) { content = Double(s.contentSize.height) }
      #else
        func find(_ v: NSView) -> NSScrollView? {
          (v as? NSScrollView) ?? v.subviews.lazy.compactMap(find).first
        }
        if let s = (window as? NSWindow)?.contentView.flatMap(find) {
          content = Double(s.documentView?.frame.height ?? -2)
        }
      #endif
      Issue.record(
        "R2PROBE flexible: content \(content) geometry \(String(describing: t.platformGeometry(window))) items \(list.snapshot.items.count) visible \(String(describing: list.lastVisibleItem()?.key))"
      )
      withExtendedLifetime((zero, window)) {}
    }
  }
#endif
