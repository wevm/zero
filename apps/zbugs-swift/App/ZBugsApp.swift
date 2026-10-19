import SwiftUI
import ZBugsUI

@main
struct ZBugsApp: App {
  // Once: each model starts a client.
  @State private var model = AppModel()

  var body: some Scene {
    WindowGroup {
      RootView(model: model)
    }
  }
}
