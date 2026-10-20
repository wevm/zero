#if canImport(SwiftUI)
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
  public struct VirtualList<
    Context: Sendable, Row: ZeroRow, Key: Hashable, StartRow: Equatable & Sendable,
    ListContext: Equatable & Sendable, RowContent: View
  >: View {
    public typealias Virtualizer = ZeroVirtualizer<Context, Row, Key, StartRow, ListContext>

    private let list: Virtualizer
    private let row: (VirtualItem<Row, Key>) -> RowContent
    @State private var host = ScrollGeometryHost()
    @State private var position = ScrollPosition()

    public init(
      _ list: Virtualizer, @ViewBuilder row: @escaping (VirtualItem<Row, Key>) -> RowContent
    ) {
      self.list = list
      self.row = row
    }

    public var body: some View {
      let snapshot = list.snapshot
      ScrollView {
        // Not lazy: the snapshot is already the rows to draw, and each must
        // be measured (`ScrollGeometryHost`).
        VStack(spacing: 0) {
          Color.clear.frame(height: snapshot.spaceBefore)
          ForEach(snapshot.items, id: \.key) { item in
            row(item)
              .onGeometryChange(for: CGRect.self) {
                $0.frame(in: .named(Self.rows))
              } action: { [host] frame in
                host.rowFrame(item.key, minY: frame.minY, maxY: frame.maxY)
              }
          }
          Color.clear.frame(height: snapshot.spaceAfter)
        }
        .coordinateSpace(.named(Self.rows))
      }
      .scrollPosition($position)
      .onScrollGeometryChange(for: Geometry.self) {
        Geometry($0)
      } action: { [host] _, g in
        host.geometry(
          contentOffset: g.offset, containerHeight: g.container, contentHeight: g.content,
          insetTop: g.top, insetBottom: g.bottom)
      }
      .onAppear {
        let render = { [host, list] in
          host.render(list.snapshot.items.map { (key: $0.key, index: $0.index) })
        }
        list.onSnapshot = render
        host.requestScroll = { y in position.scrollTo(y: y) }
        host.didScroll = { [list] in list.didScroll() }
        host.viewportDidResize = { [list] in list.viewportDidResize() }
        host.afterUpdate = { [list] in list.afterUpdate() }
        list.anchoring = .manual
        list.attach(host)
        render()
      }
      .onDisappear {
        list.onSnapshot = nil
        list.detach()
      }
    }

    private static var rows: String { "ZeroVirtual.VirtualList.rows" }
  }

  /// What the host needs of `ScrollGeometry`.
  private struct Geometry: Equatable {
    var offset, container, content, top, bottom: Double

    init(_ g: ScrollGeometry) {
      offset = g.contentOffset.y
      // The scroll view's full height, as UIKit's bounds: `containerSize`
      // already leaves out the insets, which the host takes off itself.
      container = g.visibleRect.height
      content = g.contentSize.height
      top = g.contentInsets.top
      bottom = g.contentInsets.bottom
    }
  }
#endif
