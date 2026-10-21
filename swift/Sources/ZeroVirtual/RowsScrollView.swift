#if canImport(SwiftUI) && (canImport(UIKit) || canImport(AppKit))
  import SwiftUI

  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif

  #if canImport(UIKit)
    typealias PlatformView = UIView
  #else
    typealias PlatformView = NSView
  #endif

  /// A row to draw: its key and index, the row (to tell a changed row from
  /// the same one), and the snapshot's item, for the row's view.
  struct RowsItem {
    var key: AnyHashable
    var index: Int
    var row: Any
    var item: Any
  }

  /// The rows the view draws: the snapshot's.
  struct RowsSource {
    var items: [RowsItem] = []
    var spaceBefore = 0.0
    var spaceAfter = 0.0
  }

  /// How a row's view is made and measured: hosted SwiftUI
  /// (`HostedRowViews`), or the app's own (`AppRowViews`). The rest of the
  /// layout is the same for both.
  @MainActor
  protocol RowViews: AnyObject {
    /// Views of rows that leave are kept, hidden, for rows that come.
    var reusesViews: Bool { get }
    /// The view showing `item`: `view` shown anew (the row's own when its
    /// row changed, or, when views are reused, one a row left), or a new
    /// one when it's nil.
    func view(
      for item: RowsItem, reusing view: PlatformView?, in layout: RowsLayout
    )
      -> PlatformView
    /// The view's height at `width`.
    func height(of view: PlatformView, width: Double) -> Double
    /// The view's row left and the view isn't reused.
    func discard(_ view: PlatformView)
  }

  /// A row on screen: its view, its measured height, and the row it shows.
  @MainActor
  final class RowCell {
    var view: PlatformView
    var height: Double?
    var row: Any?

    init(view: PlatformView) { self.view = view }
  }

  /// What the platform scroll views share: the rows' cells, and the layout
  /// host that places them.
  @MainActor
  final class RowsLayout {
    let host = LayoutHost()
    let views: any RowViews
    var source = RowsSource()
    private var cells: [AnyHashable: RowCell] = [:]
    /// Views rows left, hidden, for other rows (when `views` reuses them).
    private var pool: [PlatformView] = []
    private var width = 0.0
    /// Changes with each snapshot: rows are compared with their cells' then.
    private var generation = 0
    private var seen: [AnyHashable: Int] = [:]
    /// The list has drawn rows. Until then, it draws none while it opens at
    /// a jump: its first frame is where the jump lands, not where it starts.
    private(set) var shown = false
    var addView: (PlatformView) -> Void = { _ in }

    init(views: any RowViews) {
      self.views = views
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

    /// The row with `key` changed height on its own: it's measured again in
    /// the next layout pass.
    func rowHeightDidChange(_ key: AnyHashable) {
      guard let cell = cells[key] else { return }
      cell.height = nil
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
      for (key, cell) in cells where !live.contains(key) {
        cells[key] = nil
        seen[key] = nil
        if views.reusesViews {
          cell.view.isHidden = true
          pool.append(cell.view)
        } else {
          cell.view.removeFromSuperview()
          views.discard(cell.view)
        }
      }
      return (
        r.frames.compactMap { f in cells[f.key].map { ($0, f.minY, f.maxY) } }, r.contentHeight,
        r.offset
      )
    }

    private func measure(_ key: AnyHashable) -> Double {
      guard let item = source.items.first(where: { $0.key == key }) else { return 0 }
      if let cell = cells[key] {
        if seen[key] != generation {
          seen[key] = generation
          // The same row: its view and height stand.
          if let old = cell.row, !sameRow(old, item.row) {
            show(item, in: cell)
          }
        }
        return measure(cell)
      }
      let reused = views.reusesViews ? pool.popLast() : nil
      let view = views.view(for: item, reusing: reused, in: self)
      if view !== reused {
        if let reused { pool.append(reused) }
        addView(view)
      }
      let cell = RowCell(view: view)
      cell.row = item.row
      cells[key] = cell
      seen[key] = generation
      return measure(cell)
    }

    private func show(_ item: RowsItem, in cell: RowCell) {
      cell.row = item.row
      cell.height = nil
      let view = views.view(for: item, reusing: cell.view, in: self)
      guard view !== cell.view else { return }
      // A new view in place of the row's own.
      cell.view.removeFromSuperview()
      views.discard(cell.view)
      cell.view = view
      addView(view)
    }

    private func measure(_ cell: RowCell) -> Double {
      if let height = cell.height { return height }
      let h = views.height(of: cell.view, width: width)
      cell.height = h
      return h
    }

    /// The row's height as it laid itself out: when it's another than the
    /// one measured, it's measured again.
    func row(_ key: AnyHashable, laidOutAt height: Double) {
      guard let old = cells[key]?.height, abs(old - height) > 0.5 else { return }
      rowHeightDidChange(key)
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

  /// SwiftUI rows, each in its own hosting controller. A row's controller
  /// isn't reused for another row: it would show the row it had until its
  /// next layout.
  @MainActor
  final class HostedRowViews: RowViews {
    #if canImport(UIKit)
      typealias Controller = UIHostingController<AnyView>
    #else
      typealias Controller = NSHostingController<AnyView>
    #endif
    var row: (Any) -> AnyView
    private var controllers: [ObjectIdentifier: Controller] = [:]

    init(row: @escaping (Any) -> AnyView) { self.row = row }

    var reusesViews: Bool { false }

    func view(
      for item: RowsItem, reusing view: PlatformView?, in layout: RowsLayout
    )
      -> PlatformView
    {
      let controller: Controller
      if let view, let existing = controllers[ObjectIdentifier(view)] {
        controller = existing
      } else {
        controller = Controller(rootView: AnyView(EmptyView()))
        #if canImport(UIKit)
          controller.safeAreaRegions = []
          controller.view.backgroundColor = .clear
        #else
          controller.sizingOptions = []
        #endif
        controllers[ObjectIdentifier(controller.view)] = controller
      }
      let key = item.key
      controller.rootView = AnyView(
        row(item.item)
          .fixedSize(horizontal: false, vertical: true)
          .onGeometryChange(for: Double.self) {
            Double($0.size.height)
          } action: { [weak layout] h in
            // The row changed height on its own, as a view inside it loaded.
            layout?.row(key, laidOutAt: h)
          })
      return controller.view
    }

    func height(of view: PlatformView, width: Double) -> Double {
      guard let controller = controllers[ObjectIdentifier(view)] else { return 0 }
      return Double(
        controller.sizeThatFits(in: CGSize(width: width, height: .greatestFiniteMagnitude)).height)
    }

    func discard(_ view: PlatformView) {
      controllers[ObjectIdentifier(view)] = nil
    }
  }

  /// The app's own row views, measured with Auto Layout at the list's
  /// width, and reused.
  @MainActor
  final class AppRowViews: RowViews {
    let make: (Any, PlatformView?) -> PlatformView

    init(make: @escaping (Any, PlatformView?) -> PlatformView) { self.make = make }

    var reusesViews: Bool { true }

    func view(
      for item: RowsItem, reusing view: PlatformView?, in layout: RowsLayout
    )
      -> PlatformView
    {
      make(item.item, view)
    }

    func height(of view: PlatformView, width: Double) -> Double {
      #if canImport(UIKit)
        let size = view.systemLayoutSizeFitting(
          CGSize(width: width, height: UIView.layoutFittingCompressedSize.height),
          withHorizontalFittingPriority: .required, verticalFittingPriority: .fittingSizeLevel)
        return Double(size.height)
      #else
        let constraint = view.widthAnchor.constraint(equalToConstant: width)
        constraint.isActive = true
        defer { constraint.isActive = false }
        // Laid out at the width first: a wrapping label wraps at the width
        // it was laid out at, and its height follows.
        view.setFrameSize(NSSize(width: width, height: view.frame.height))
        view.layoutSubtreeIfNeeded()
        return Double(view.fittingSize.height)
      #endif
    }

    func discard(_ view: PlatformView) {}
  }

  #if canImport(UIKit)
    /// A scroll view that lays out the rows itself: their frames, its
    /// content size and its offset, all in `layoutSubviews`.
    final class RowsScrollView: UIScrollView {
      let rows: RowsLayout
      private var laying = false

      init(views: any RowViews) {
        rows = RowsLayout(views: views)
        super.init(frame: .zero)
        alwaysBounceVertical = true
        rows.host.setNeedsLayout = { [weak self] in self?.setNeedsLayout() }
        rows.addView = { [weak self] view in self?.addSubview(view) }
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
      let rows: RowsLayout
      private let document = RowsDocumentView()
      private var laying = false

      init(views: any RowViews) {
        rows = RowsLayout(views: views)
        super.init(frame: .zero)
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
        rows.addView = { [weak self] view in self?.document.addSubview(view) }
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
    let row: (Any) -> AnyView
    let model: VirtualListModel

    func make() -> RowsScrollView {
      let view = RowsScrollView(views: HostedRowViews(row: row))
      model.source = source
      model.connect(view)
      view.rows.update(source())
      return view
    }

    func update(_ view: RowsScrollView) {
      model.source = source
      (view.rows.views as? HostedRowViews)?.row = row
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
