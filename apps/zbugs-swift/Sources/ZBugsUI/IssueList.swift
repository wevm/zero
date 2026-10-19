#if canImport(SwiftUI)
  import Foundation
  import SwiftUI
  import ZBugs
  import Zero
  import ZeroUI

  public enum IssueStatus: String, CaseIterable, Identifiable, Sendable {
    case open, closed, all
    public var id: Self { self }
    var title: String { rawValue.capitalized }
  }

  /// What the issue list shows, as zbugs' list page reads it from the URL.
  public struct IssueFilter: Hashable, Sendable {
    public var status = IssueStatus.open
    public var text = ""
    public var sortField = SortField.modified
    public var sortDirection = AST.Direction.desc
    public var creator: String?
    public var assignee: String?

    public init() {}

    var listContext: ListContextParams {
      ListContextParams(
        open: status == .open ? true : status == .closed ? false : nil,
        projectName: AppModel.projectName, assignee: assignee, creator: creator, labels: [],
        textFilter: text.isEmpty ? nil : text, sortField: sortField,
        sortDirection: sortDirection)
    }

    /// The first page of the list.
    var query: QueryRequest<Issue, [Issue], AuthData?> {
      Queries.issueListV2(
        IssueListV2Args(
          listContext: listContext, limit: 200, start: nil, dir: .forward, inclusive: true))
    }
  }

  /// The issue list in SwiftUI.
  struct IssueList: View {
    @ZeroQuery<Issue, [Issue], AuthData?> var issues: [Issue]
    @Binding var selection: String?

    init(filter: IssueFilter, selection: Binding<String?>) {
      _issues = ZeroQuery(filter.query)
      _selection = selection
    }

    var body: some View {
      List(issues, selection: $selection) { issue in
        IssueRow(issue: issue).tag(issue.id)
      }
      .overlay {
        if issues.isEmpty {
          if $issues.resultType == .complete {
            ContentUnavailableView("No Issues", systemImage: "checkmark.circle")
          } else {
            ProgressView()
          }
        }
      }
    }
  }

  struct IssueRow: View {
    let issue: Issue

    var body: some View {
      VStack(alignment: .leading, spacing: 4) {
        HStack(alignment: .firstTextBaseline) {
          Image(systemName: issue.open ? "circle" : "checkmark.circle.fill")
            .foregroundStyle(issue.open ? Color.green : Color.purple)
          Text(issue.title).font(.headline).lineLimit(2)
        }
        HStack(spacing: 6) {
          Text(issue.subtitle)
          ForEach(issue.labels ?? [], id: \.id) { label in
            Text(label.name)
              .padding(.horizontal, 6)
              .background(.quaternary, in: Capsule())
          }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
      }
      .padding(.vertical, 2)
      .accessibilityElement(children: .combine)
      .accessibilityIdentifier("IssueRow")
    }
  }

  extension Issue {
    /// `#123 · alice · 3 days ago`.
    var subtitle: String {
      var parts: [String] = []
      if let shortID { parts.append("#\(shortID)") }
      if let creator { parts.append(creator.login) }
      parts.append(
        Date(timeIntervalSince1970: modified / 1000).formatted(.relative(presentation: .named)))
      return parts.joined(separator: " · ")
    }
  }
#endif
