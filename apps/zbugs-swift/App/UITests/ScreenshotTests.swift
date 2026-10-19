import XCTest

/// Drives the app against zbugs' backend (`server/server.ts`) and keeps a
/// screenshot of each screen, which CI uploads. `ZBUGS_TOKEN` is a token for
/// the seeded crew user; xcodebuild passes `TEST_RUNNER_ZBUGS_TOKEN` as it.
///
/// A step whose screen doesn't appear records a failure and the test goes
/// on, so one run shows every screen it can reach.
final class ScreenshotTests: XCTestCase {
  @MainActor
  func testScreens() throws {
    let token = try XCTUnwrap(
      ProcessInfo.processInfo.environment["ZBUGS_TOKEN"], "Set TEST_RUNNER_ZBUGS_TOKEN")
    let app = XCUIApplication()
    app.launchEnvironment["ZBUGS_TOKEN"] = token
    app.launch()

    // The issue list, once zero-cache has synced it.
    let firstIssue = app.cells.firstMatch
    guard expect(firstIssue, "an issue in the list", timeout: 120) else {
      snapshot("1 Issues (empty)")
      return
    }
    settle()
    snapshot("1 Issues")

    // An issue with its comments.
    firstIssue.tap()
    if expect(app.buttons["Comment"], "the issue's comment button") {
      settle()
      snapshot("2 Issue")
    }

    // A new issue.
    if UIDevice.current.userInterfaceIdiom == .phone {
      back(app)
    }
    let newIssue = app.buttons["New Issue"]
    if expect(newIssue, "the New Issue button") {
      newIssue.tap()
      if expect(app.navigationBars["New Issue"], "the new issue sheet") {
        settle()
        snapshot("3 New Issue")
        app.buttons["Cancel"].tap()
      }
    }

    // The filters, then the UIKit list.
    if UIDevice.current.userInterfaceIdiom == .phone {
      back(app)
    }
    let listPicker = app.buttons["List"]
    if expect(listPicker, "the filters' List picker") {
      settle()
      snapshot("4 Filters")
      listPicker.tap()
      let uiKit = app.buttons["UIKit"]
      if expect(uiKit, "the UIKit option") {
        uiKit.tap()
      }
      if UIDevice.current.userInterfaceIdiom == .phone {
        let issues = app.buttons["Issues"]
        if expect(issues, "the Issues button") { issues.tap() }
      }
      if expect(app.tables.cells.firstMatch, "an issue in the UIKit list") {
        settle()
        snapshot("5 Issues (UIKit)")
      }
    }
  }

  /// Waits for `element`, recording a failure when it doesn't appear.
  @MainActor
  private func expect(_ element: XCUIElement, _ what: String, timeout: Double = 20) -> Bool {
    if element.waitForExistence(timeout: timeout) { return true }
    snapshot("Missing \(what)")
    XCTFail("No \(what)")
    return false
  }

  /// The navigation bar's back button.
  @MainActor
  private func back(_ app: XCUIApplication) {
    let button = app.navigationBars.buttons.element(boundBy: 0)
    if button.waitForExistence(timeout: 5) { button.tap() }
  }

  /// Lets animations and the rows' relationships finish.
  private func settle() {
    Thread.sleep(forTimeInterval: 2)
  }

  @MainActor
  private func snapshot(_ name: String) {
    let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    attachment.name = name
    attachment.lifetime = .keepAlways
    add(attachment)
  }
}
