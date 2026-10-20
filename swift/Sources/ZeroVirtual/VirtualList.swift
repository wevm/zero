#if canImport(SwiftUI) && (canImport(UIKit) || canImport(AppKit))
  import SwiftUI
  import Zero

  /// A scroll view of a `ZeroVirtualizer`'s rows: they page in as it
  /// scrolls, and what's on screen stays put as rows load, go, and change
  /// height.
  ///
  /// ```swift
  /// VirtualList(messages) { item in
  ///   MessageRow(message: item.row)
  /// }
  /// ```
  ///
  /// Rows can be any height, and change it. The list draws only the rows
  /// near the screen, with space standing in for the rest.
  ///
  /// Underneath is the platform's scroll view (`UIScrollView`,
  /// `NSScrollView`), which lays the rows out itself: their frames, its
  /// content's size and its offset are set in one layout pass, so no frame
  /// shows rows moved by a page loading before the offset that keeps them
  /// in place. Each row is hosted in its own hosting view: it gets the
  /// platform's traits (color scheme, Dynamic Type, layout direction), but
  /// not the list's custom environment values. Apply those in `row`.
  public struct VirtualList<
    Context: Sendable, Row: ZeroRow, Key: Hashable, StartRow: Equatable & Sendable,
    ListContext: Equatable & Sendable, RowContent: View
  >: View {
    public typealias Virtualizer = ZeroVirtualizer<Context, Row, Key, StartRow, ListContext>

    private let list: Virtualizer
    private let row: (VirtualItem<Row, Key>) -> RowContent
    @State private var model = VirtualListModel()

    public init(
      _ list: Virtualizer, @ViewBuilder row: @escaping (VirtualItem<Row, Key>) -> RowContent
    ) {
      self.list = list
      self.row = row
    }

    public var body: some View {
      // Read here, so the list renders each snapshot.
      _ = list.snapshot
      return RowsScrollViewRepresentable(source: source, model: model)
        .onAppear { model.attach(list) }
        .onDisappear { model.detach(list) }
    }

    /// The snapshot's rows, as the scroll view draws them.
    private var source: () -> RowsSource {
      { [list, row] in
        let snapshot = list.snapshot
        return RowsSource(
          items: snapshot.items.map { item in
            RowsItem(
              key: AnyHashable(item.key), index: item.index, row: item.row,
              view: { AnyView(row(item)) })
          },
          spaceBefore: snapshot.spaceBefore, spaceAfter: snapshot.spaceAfter)
      }
    }
  }

  /// Ties the virtualizer to the scroll view: attached while the list is
  /// on screen.
  @MainActor
  final class VirtualListModel {
    weak var view: RowsScrollView?
    var source: () -> RowsSource = { RowsSource() }
    private var attachAs: ((RowsScrollView) -> Void)?
    private var attached = false

    func attach<C, R, K, S, L>(_ list: ZeroVirtualizer<C, R, K, S, L>) {
      attachAs = { [unowned self] view in
        let host = view.rows.host
        host.didScroll = { [list] in list.didScroll() }
        host.viewportDidResize = { [list] in list.viewportDidResize() }
        host.afterUpdate = { [list] in list.afterUpdate() }
        host.isLanding = { [list] in list.isLanding }
        list.anchoring = .manual
        // Each snapshot, in the layout pass or out of it, goes to the view
        // at once: a pass lays out what the core publishes during it.
        list.onSnapshot = { [weak view, unowned self] in view?.rows.update(source()) }
        list.attach(host)
        view.rows.update(source())
      }
      if let view { connect(view) }
    }

    func detach<C, R, K, S, L>(_ list: ZeroVirtualizer<C, R, K, S, L>) {
      attachAs = nil
      attached = false
      list.onSnapshot = nil
      list.detach()
    }

    /// The view exists: attach to it if the list is on screen.
    func connect(_ view: RowsScrollView) {
      self.view = view
      guard !attached, let attachAs else { return }
      attached = true
      attachAs(view)
    }
  }
#endif
