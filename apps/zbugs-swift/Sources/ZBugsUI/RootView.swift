#if canImport(SwiftUI)
  import SwiftUI
  import ZBugs
  import Zero
  import ZeroUI

  /// Which UI layer draws the issue list.
  public enum ListLayer: String, CaseIterable, Identifiable, Sendable {
    case swiftUI = "SwiftUI"
    #if canImport(UIKit)
      case native = "UIKit"
    #else
      case native = "AppKit"
    #endif
    public var id: Self { self }
  }

  /// The app: filters, the issue list, and the selected issue.
  public struct RootView: View {
    @State private var model = AppModel()
    @State private var filter = IssueFilter()
    @State private var selection: String?
    @State private var listLayer = ListLayer.swiftUI
    @State private var sheet: Sheet?

    enum Sheet: String, Identifiable {
      case newIssue, login, servers
      var id: Self { self }
    }

    public init() {}

    public var body: some View {
      NavigationSplitView {
        Sidebar(filter: $filter, listLayer: $listLayer)
          .navigationTitle("Zero Bugs")
      } content: {
        list
          .navigationTitle(filter.status.title + " Issues")
          .searchable(text: $filter.text, prompt: "Search issues")
          .toolbar { toolbar }
      } detail: {
        if let selection {
          IssueDetail(id: selection).id(selection)
        } else {
          ContentUnavailableView("No Issue Selected", systemImage: "list.bullet.rectangle")
        }
      }
      .zero(model.zero)
      .environment(model)
      .sheet(item: $sheet) { sheet in
        Group {
          switch sheet {
          case .newIssue: NewIssue { selection = $0 }
          case .login: LoginSheet()
          case .servers: ServerSettings()
          }
        }
        .environment(model)
        .zero(model.zero)
      }
      .alert(
        "Couldn't Save",
        isPresented: Binding {
          model.mutationError != nil
        } set: { _ in
          model.mutationError = nil
        }
      ) {
      } message: {
        Text(model.mutationError ?? "")
      }
    }

    @ViewBuilder private var list: some View {
      switch listLayer {
      case .swiftUI:
        IssueList(filter: filter, selection: $selection)
      case .native:
        IssueTable(zero: model.zero, filter: filter, selection: $selection)
      }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
      ToolbarItem {
        ConnectionBadge(connection: model.zero.connection)
      }
      ToolbarItem {
        Button("New Issue", systemImage: "square.and.pencil") { sheet = .newIssue }
          .disabled(model.auth == nil)
      }
      ToolbarItem {
        Menu("Account", systemImage: "person.crop.circle") {
          if let auth = model.auth {
            Text("Logged in as \(auth.sub) (\(auth.role.rawValue))")
            Button("Log Out") { model.logOut() }
          } else {
            Button("Log In…") { sheet = .login }
          }
          Button("Servers…") { sheet = .servers }
        }
      }
    }
  }

  private struct Sidebar: View {
    @Binding var filter: IssueFilter
    @Binding var listLayer: ListLayer
    @Environment(AppModel.self) private var model

    var body: some View {
      Form {
        Picker("Status", selection: $filter.status) {
          ForEach(IssueStatus.allCases) { Text($0.title).tag($0) }
        }
        Picker("Sort", selection: $filter.sortField) {
          Text("Modified").tag(SortField.modified)
          Text("Created").tag(SortField.created)
        }
        Picker("Order", selection: $filter.sortDirection) {
          Text("Newest First").tag(AST.Direction.desc)
          Text("Oldest First").tag(AST.Direction.asc)
        }
        if let auth = model.auth {
          MineToggles(filter: $filter, userID: auth.sub)
        }
        Picker("List", selection: $listLayer) {
          ForEach(ListLayer.allCases) { Text($0.rawValue).tag($0) }
        }
      }
      .formStyle(.grouped)
    }
  }

  /// Filters by the logged-in user, whose login the list matches.
  private struct MineToggles: View {
    @Binding var filter: IssueFilter
    @ZeroQuery<User, User?, AuthData?> var user: User?

    init(filter: Binding<IssueFilter>, userID: String) {
      _filter = filter
      _user = ZeroQuery(Queries.user(userID))
    }

    var body: some View {
      if let login = user?.login {
        Toggle(
          "Created by Me",
          isOn: Binding {
            filter.creator == login
          } set: {
            filter.creator = $0 ? login : nil
          })
        Toggle(
          "Assigned to Me",
          isOn: Binding {
            filter.assignee == login
          } set: {
            filter.assignee = $0 ? login : nil
          })
      }
    }
  }

  /// The connection state, as a dot.
  private struct ConnectionBadge: View {
    let connection: Connection

    var body: some View {
      let (color, text): (Color, String) =
        switch connection.state {
        case .connected: (.green, "Connected")
        case .connecting, .initializing: (.yellow, "Connecting")
        case .needsAuth: (.orange, "Log In Again")
        case .disconnected, .error, .closed: (.red, "Offline")
        }
      SwiftUI.Label {
        Text(text)
      } icon: {
        Image(systemName: "circle.fill").foregroundStyle(color)
      }
      .labelStyle(.titleAndIcon)
      .font(.caption)
      .help(String(describing: connection.state))
    }
  }
#endif
