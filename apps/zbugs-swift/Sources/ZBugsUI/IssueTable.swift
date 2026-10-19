#if canImport(SwiftUI)
  import SwiftUI
  import ZBugs
  import Zero
  import ZeroUI

  #if canImport(UIKit)
    import UIKit

    /// The issue list in UIKit: a table view fed by `DiffableAdapter`.
    final class IssueTableController: UITableViewController {
      var onSelect: (String) -> Void = { _ in }
      private var adapter: DiffableAdapter<Issue, Int>?
      private var dataSource: UITableViewDiffableDataSource<Int, RowID>?
      private var key: (zero: ObjectIdentifier, filter: IssueFilter)?

      override func viewDidLoad() {
        super.viewDidLoad()
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "issue")
        dataSource = UITableViewDiffableDataSource(tableView: tableView) {
          [unowned self] tableView, indexPath, id in
          let cell = tableView.dequeueReusableCell(withIdentifier: "issue", for: indexPath)
          var content = UIListContentConfiguration.subtitleCell()
          if let issue = adapter?.row(for: id) {
            content.text = issue.title
            content.secondaryText = issue.subtitle
            content.image = UIImage(systemName: issue.open ? "circle" : "checkmark.circle.fill")
            content.imageProperties.tintColor = issue.open ? .systemGreen : .systemPurple
          }
          cell.contentConfiguration = content
          cell.accessibilityIdentifier = "IssueCell"
          return cell
        }
      }

      /// Shows `filter`'s issues; nothing to do when they are showing.
      func show(_ zero: Zero<AuthData?>, _ filter: IssueFilter) {
        loadViewIfNeeded()
        if let key, key.zero == ObjectIdentifier(zero), key.filter == filter { return }
        key = (ObjectIdentifier(zero), filter)
        guard let dataSource, let view = try? zero.materialize(filter.query) else { return }
        adapter = DiffableAdapter(view, section: 0, dataSource: dataSource)
      }

      override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        guard let id = dataSource?.itemIdentifier(for: indexPath),
          let issue = adapter?.row(for: id)
        else { return }
        onSelect(issue.id)
      }
    }

    struct IssueTable: UIViewControllerRepresentable {
      let zero: Zero<AuthData?>
      let filter: IssueFilter
      @Binding var selection: String?

      func makeUIViewController(context: Context) -> IssueTableController {
        IssueTableController(style: .plain)
      }

      func updateUIViewController(_ controller: IssueTableController, context: Context) {
        controller.onSelect = { selection = $0 }
        controller.show(zero, filter)
      }
    }
  #elseif canImport(AppKit)
    import AppKit

    /// The issue list in AppKit: a table view fed by `DiffableAdapter`.
    final class IssueTableController: NSViewController, NSTableViewDelegate {
      var onSelect: (String) -> Void = { _ in }
      private let tableView = NSTableView()
      private var adapter: DiffableAdapter<Issue, Int>?
      private var dataSource: NSTableViewDiffableDataSource<Int, RowID>?
      private var key: (zero: ObjectIdentifier, filter: IssueFilter)?

      override func loadView() {
        let column = NSTableColumn(identifier: .init("issue"))
        column.title = "Issue"
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.usesAutomaticRowHeights = true
        tableView.style = .inset
        tableView.delegate = self
        dataSource = NSTableViewDiffableDataSource(tableView: tableView) {
          [unowned self] _, _, _, id in
          let cell = NSTableCellView()
          cell.setAccessibilityIdentifier("IssueCell")
          let title = NSTextField(labelWithString: "")
          title.font = .preferredFont(forTextStyle: .headline)
          title.lineBreakMode = .byTruncatingTail
          let subtitle = NSTextField(labelWithString: "")
          subtitle.font = .preferredFont(forTextStyle: .caption1)
          subtitle.textColor = .secondaryLabelColor
          if let issue = adapter?.row(for: id) {
            title.stringValue = (issue.open ? "○ " : "✓ ") + issue.title
            subtitle.stringValue = issue.subtitle
          }
          let stack = NSStackView(views: [title, subtitle])
          stack.orientation = .vertical
          stack.alignment = .leading
          stack.spacing = 2
          stack.edgeInsets = NSEdgeInsets(top: 4, left: 0, bottom: 4, right: 0)
          stack.translatesAutoresizingMaskIntoConstraints = false
          cell.addSubview(stack)
          NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: cell.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: cell.trailingAnchor),
            stack.topAnchor.constraint(equalTo: cell.topAnchor),
            stack.bottomAnchor.constraint(equalTo: cell.bottomAnchor),
          ])
          return cell
        }
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.hasVerticalScroller = true
        view = scroll
      }

      /// Shows `filter`'s issues; nothing to do when they are showing.
      func show(_ zero: Zero<AuthData?>, _ filter: IssueFilter) {
        loadViewIfNeeded()
        if let key, key.zero == ObjectIdentifier(zero), key.filter == filter { return }
        key = (ObjectIdentifier(zero), filter)
        guard let dataSource, let view = try? zero.materialize(filter.query) else { return }
        adapter = DiffableAdapter(view, section: 0, dataSource: dataSource)
      }

      func tableViewSelectionDidChange(_ notification: Notification) {
        guard tableView.selectedRow >= 0,
          let id = dataSource?.itemIdentifier(forRow: tableView.selectedRow),
          let issue = adapter?.row(for: id)
        else { return }
        onSelect(issue.id)
      }
    }

    struct IssueTable: NSViewControllerRepresentable {
      let zero: Zero<AuthData?>
      let filter: IssueFilter
      @Binding var selection: String?

      func makeNSViewController(context: Context) -> IssueTableController {
        IssueTableController()
      }

      func updateNSViewController(_ controller: IssueTableController, context: Context) {
        controller.onSelect = { selection = $0 }
        controller.show(zero, filter)
      }
    }
  #endif
#endif
