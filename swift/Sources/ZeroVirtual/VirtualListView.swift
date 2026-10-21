#if canImport(SwiftUI) && (canImport(UIKit) || canImport(AppKit))
  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif
  import Zero

  #if canImport(UIKit)
    /// What `VirtualListView` is: a `UIView`.
    public typealias VirtualListViewBase = UIView
  #else
    /// What `VirtualListView` is: an `NSView`.
    public typealias VirtualListViewBase = NSView
  #endif

  /// `VirtualList` for UIKit and AppKit: a scroll view of a
  /// `ZeroVirtualizer`'s rows, which are the app's own views. They page in
  /// as it scrolls, and what's on screen stays put as rows load, go, and
  /// change height.
  ///
  /// ```swift
  /// let list = VirtualListView(messages) { item, reusing in
  ///   let row = reusing as? MessageRowView ?? MessageRowView()
  ///   row.show(item.row)
  ///   return row
  /// }
  /// ```
  ///
  /// `row` makes the view of a row, or shows the row in `reusing`, a view
  /// another row left; it's nil when there's none. A row whose data
  /// changes (rows of an `Equatable` type that no longer compare equal) is
  /// shown again in its own view. A row's height is its view's, laid out
  /// with Auto Layout at the list's width: its constraints must determine
  /// it. A row that changes height on its own, not with its data, says so
  /// with `rowHeightDidChange(_:)`.
  ///
  /// The list lays the rows out in its `scrollView` itself: their frames,
  /// the content's size and the offset are set in one layout pass, so no
  /// frame shows rows moved by a page loading before the offset that keeps
  /// them in place. Set the scroll view's insets and delegate as for any
  /// other, but not its content size or offset; scroll with the
  /// virtualizer (`scrollToItem`). The list follows the virtualizer while
  /// it's in a window.
  public final class VirtualListView<
    Context: Sendable, Row: ZeroRow, Key: Hashable, StartRow: Equatable & Sendable,
    ListContext: Equatable & Sendable
  >: VirtualListViewBase {
    public typealias Virtualizer = ZeroVirtualizer<Context, Row, Key, StartRow, ListContext>

    private let list: Virtualizer
    private let rows: RowsScrollView
    private let model = VirtualListModel()

    #if canImport(UIKit)
      /// The scroll view the rows are in.
      public var scrollView: UIScrollView { rows }
    #else
      /// The scroll view the rows are in.
      public var scrollView: NSScrollView { rows }
    #endif

    #if canImport(UIKit)
      public convenience init(
        _ list: Virtualizer,
        row: @escaping @MainActor (_ item: VirtualItem<Row, Key>, _ reusing: UIView?) -> UIView
      ) {
        self.init(list, make: row)
      }
    #else
      public convenience init(
        _ list: Virtualizer,
        row: @escaping @MainActor (_ item: VirtualItem<Row, Key>, _ reusing: NSView?) -> NSView
      ) {
        self.init(list, make: row)
      }
    #endif

    private init(
      _ list: Virtualizer,
      make row: @escaping @MainActor (VirtualItem<Row, Key>, PlatformView?) -> PlatformView
    ) {
      self.list = list
      rows = RowsScrollView(
        views: AppRowViews { item, reusing in row(item as! VirtualItem<Row, Key>, reusing) })
      super.init(frame: .zero)
      #if canImport(UIKit)
        rows.autoresizingMask = [.flexibleWidth, .flexibleHeight]
      #else
        rows.autoresizingMask = [.width, .height]
      #endif
      rows.frame = bounds
      addSubview(rows)
      model.source = { [list] in RowsSource(list.snapshot) }
      model.connect(rows)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

    /// The row with `key` changed height on its own: its view is measured
    /// again, and the rows below it move, with what's on screen staying
    /// put.
    public func rowHeightDidChange(_ key: Key) {
      rows.rows.rowHeightDidChange(AnyHashable(key))
    }

    #if canImport(UIKit)
      public override func didMoveToWindow() {
        super.didMoveToWindow()
        follow(window != nil)
      }
    #else
      public override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        follow(window != nil)
      }
    #endif

    private func follow(_ inWindow: Bool) {
      if inWindow { model.attach(list) } else { model.detach(list) }
    }
  }
#endif
