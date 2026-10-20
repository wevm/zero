#if canImport(SwiftUI) && (canImport(UIKit) || canImport(AppKit))
  import SwiftUI

  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif

  /// A row to draw: its key and index, the row (to tell a changed row from
  /// the same one), and its view.
  struct RowsItem {
    var key: AnyHashable
    var index: Int
    var row: Any
    var view: () -> AnyView
  }

  /// The rows the view draws: the snapshot's.
  struct RowsSource {
    var items: [RowsItem] = []
    var spaceBefore = 0.0
    var spaceAfter = 0.0
  }

  /// A row's hosted SwiftUI view and its measured height.
  @MainActor
  final class RowCell {
    #if canImport(UIKit)
      let controller = UIHostingController(rootView: AnyView(EmptyView()))
      var view: UIView { controller.view }
    #else
      let controller = NSHostingController(rootView: AnyView(EmptyView()))
      var view: NSView { controller.view }
    #endif
    var height: Double?
    var row: Any?

    init() {
      #if canImport(UIKit)
        controller.safeAreaRegions = []
        controller.view.backgroundColor = .clear
      #else
        controller.sizingOptions = []
      #endif
    }

    func measure(width: Double) -> Double {
      if let height { return height }
      let size = controller.sizeThatFits(
        in: CGSize(width: width, height: .greatestFiniteMagnitude))
      let h = Double(size.height)
      height = h
      return h
    }
  }

  /// What the platform scroll views share: the rows' cells, kept and
  /// reused, and the layout host that places them.
  @MainActor
  final class RowsLayout {
    let host = LayoutHost()
    var source = RowsSource()
    private var cells: [AnyHashable: RowCell] = [:]
    private var width = 0.0
    /// Changes with each snapshot: rows are compared with their cells' then.
    private var generation = 0
    private var seen: [AnyHashable: Int] = [:]
    /// The list has drawn rows. Until then, it draws none while it opens at
    /// a jump: its first frame is where the jump lands, not where it starts.
    private(set) var shown = false
    var addCell: (RowCell) -> Void = { _ in }

    init() {
      host.rows = { [unowned self] in
        (source.items.map { (key: $0.key, index: $0.index) }, source.spaceBefore, source.spaceAfter)
      }
      host.measure = { [unowned self] key in measure(key) }
    }

    /// A new snapshot to draw.
    func update(_ source: RowsSource) {
      self.source = source
      generation += 1
      host.invalidate()
    }

    /// One layout pass at `width`: the cells to place, the content's height
    /// and the offset.
    func layout(
      width: Double, offset: Double, viewport: Double
    ) -> (
      frames: [(cell: RowCell, minY: Double, maxY: Double)], contentHeight: Double, offset: Double
    ) {
      if width != self.width {
        self.width = width
        for cell in cells.values { cell.height = nil }
        host.invalidate()
      }
      let r = host.layout(offset: offset, viewport: viewport)
      if !shown, !r.frames.isEmpty, !host.isLanding() { shown = true }
      let live = Set(r.frames.map(\.key))
      // A row's view isn't reused for another row: it would show the row it
      // had until its next layout.
      for (key, cell) in cells where !live.contains(key) {
        cells[key] = nil
        seen[key] = nil
        cell.view.removeFromSuperview()
      }
      return (
        r.frames.compactMap { f in cells[f.key].map { ($0, f.minY, f.maxY) } }, r.contentHeight,
        r.offset
      )
    }

    private func measure(_ key: AnyHashable) -> Double {
      guard let item = source.items.first(where: { $0.key == key }) else { return 0 }
      let cell: RowCell
      if let existing = cells[key] {
        cell = existing
      } else {
        cell = RowCell()
        addCell(cell)
        cells[key] = cell
      }
      if seen[key] != generation {
        seen[key] = generation
        if let old = cell.row, sameRow(old, item.row) {
          // The same row: its view and height stand.
        } else {
          cell.row = item.row
          cell.height = nil
          cell.controller.rootView = AnyView(
            item.view()
              .fixedSize(horizontal: false, vertical: true)
              .onGeometryChange(for: Double.self) {
                Double($0.size.height)
              } action: {
                [weak self, weak cell] h in
                // The row changed height on its own, as a view inside it
                // loaded.
                guard let self, let cell, let old = cell.height, abs(old - h) > 0.5 else { return }
                cell.height = nil
                self.host.invalidate()
              })
        }
      }
      return cell.measure(width: width)
    }
  }

  /// Rows of an `Equatable` type that compare equal are the same row.
  private func sameRow(_ a: Any, _ b: Any) -> Bool {
    guard let a = a as? any Equatable else { return false }
    return a.isEqual(to: b)
  }

  extension Equatable {
    fileprivate func isEqual(to other: Any) -> Bool { (other as? Self) == self }
  }

  #if canImport(UIKit)
    /// A scroll view that lays out the rows itself: their frames, its
    /// content size and its offset, all in `layoutSubviews`.
    final class RowsScrollView: UIScrollView {
      let rows = RowsLayout()
      private var laying = false

      override init(frame: CGRect) {
        super.init(frame: frame)
        alwaysBounceVertical = true
        rows.host.setNeedsLayout = { [weak self] in self?.setNeedsLayout() }
        rows.addCell = { [weak self] cell in self?.addSubview(cell.view) }
      }

      required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

      override func layoutSubviews() {
        super.layoutSubviews()
        // Applying the offset below lays the view out again.
        guard !laying else { return }
        laying = true
        defer { laying = false }
        let inset = adjustedContentInset
        let width = Double(bounds.width - inset.left - inset.right)
        let r = rows.layout(
          width: width, offset: Double(contentOffset.y + inset.top),
          viewport: Double(bounds.height - inset.top - inset.bottom))
        let scale = max(1, window?.screen.scale ?? traitCollection.displayScale)
        func align(_ y: Double) -> CGFloat { (CGFloat(y) * scale).rounded() / scale }
        for f in r.frames {
          let frame = CGRect(
            x: 0, y: align(f.minY), width: width, height: align(f.maxY) - align(f.minY))
          f.cell.view.isHidden = !rows.shown
          // Set only when it moves: a frame set lays the row's view out again.
          guard f.cell.view.frame != frame else { continue }
          f.cell.view.frame = frame
          f.cell.view.layoutIfNeeded()
        }
        let size = CGSize(width: width, height: r.contentHeight)
        if contentSize != size { contentSize = size }
        let y = CGFloat(r.offset) - inset.top
        if contentOffset.y != y { contentOffset.y = y }
      }
    }
  #else
    /// A scroll view that lays out the rows itself: their frames, its
    /// content's size and its offset, all in one pass. The pass runs when
    /// the clip view moves or resizes (the user's scroll, before it's
    /// drawn) and in the document view's `layout` (a new snapshot).
    final class RowsScrollView: NSScrollView {
      let rows = RowsLayout()
      private let document = RowsDocumentView()
      private var laying = false

      override init(frame: CGRect) {
        super.init(frame: frame)
        hasVerticalScroller = true
        drawsBackground = false
        documentView = document
        document.owner = self
        contentView.postsBoundsChangedNotifications = true
        contentView.postsFrameChangedNotifications = true
        for name in [NSView.boundsDidChangeNotification, NSView.frameDidChangeNotification] {
          NotificationCenter.default.addObserver(
            self, selector: #selector(clipChanged), name: name, object: contentView)
        }
        rows.host.setNeedsLayout = { [weak self] in self?.document.needsLayout = true }
        rows.addCell = { [weak self] cell in self?.document.addSubview(cell.view) }
      }

      required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

      @objc private func clipChanged() { layoutRows() }

      fileprivate func layoutRows() {
        // Applying the size and offset below moves the clip view again.
        guard !laying, window != nil else { return }
        laying = true
        defer { laying = false }
        let clip = contentView
        let inset = clip.contentInsets
        let width = Double(clip.bounds.width)
        let r = rows.layout(
          width: width, offset: Double(clip.bounds.origin.y + inset.top),
          viewport: Double(clip.bounds.height - inset.top - inset.bottom))
        // On whole pixels of the content, not of the window: aligned in the
        // window, a row would snap differently at each offset and move by a
        // pixel against the content as it scrolls.
        let scale = max(1, window?.backingScaleFactor ?? 1)
        func align(_ y: Double) -> CGFloat { (CGFloat(y) * scale).rounded() / scale }
        for f in r.frames {
          let frame = CGRect(
            x: 0, y: align(f.minY), width: width, height: align(f.maxY) - align(f.minY))
          f.cell.view.isHidden = !rows.shown
          // Set only when it moves: a frame set lays the row's view out again.
          guard f.cell.view.frame != frame else { continue }
          f.cell.view.frame = frame
          f.cell.view.layoutSubtreeIfNeeded()
        }
        let size = NSSize(width: width, height: r.contentHeight)
        if document.frame.size != size { document.setFrameSize(size) }
        let y = CGFloat(r.offset) - inset.top
        if clip.bounds.origin.y != y {
          clip.scroll(to: NSPoint(x: clip.bounds.origin.x, y: y))
          reflectScrolledClipView(clip)
        }
      }
    }

    final class RowsDocumentView: NSView {
      weak var owner: RowsScrollView?

      override var isFlipped: Bool { true }

      override func layout() {
        super.layout()
        owner?.layoutRows()
      }
    }
  #endif

  /// `VirtualList`'s scroll view.
  @MainActor
  struct RowsScrollViewRepresentable {
    let source: () -> RowsSource
    let model: VirtualListModel

    func make() -> RowsScrollView {
      let view = RowsScrollView(frame: .zero)
      model.source = source
      model.connect(view)
      view.rows.update(source())
      return view
    }

    func update(_ view: RowsScrollView) {
      model.source = source
      view.rows.update(source())
    }
  }

  #if canImport(UIKit)
    extension RowsScrollViewRepresentable: UIViewRepresentable {
      func makeUIView(context: Context) -> RowsScrollView { make() }
      func updateUIView(_ view: RowsScrollView, context: Context) { update(view) }
    }
  #else
    extension RowsScrollViewRepresentable: NSViewRepresentable {
      func makeNSView(context: Context) -> RowsScrollView { make() }
      func updateNSView(_ view: RowsScrollView, context: Context) { update(view) }
    }
  #endif
#endif
