// swift-tools-version: 6.2
import PackageDescription

let package = Package(
  name: "Zero",
  platforms: [.iOS(.v26), .macOS(.v26)],
  products: [
    .library(name: "Zero", targets: ["Zero"]),
    .library(name: "ZeroUI", targets: ["ZeroUI"]),
    .library(name: "ZeroTesting", targets: ["ZeroTesting"]),
  ],
  dependencies: [
    // Test-only: the WebSocket transport of the Linux end-to-end tests.
    .package(url: "https://github.com/apple/swift-nio.git", from: "2.104.0")
  ],
  targets: [
    .target(name: "Zero"),
    .target(name: "ZeroUI", dependencies: ["Zero"]),
    .target(name: "ZeroTesting", dependencies: ["Zero"]),
    .testTarget(name: "ZeroTests", dependencies: ["Zero", "ZeroTesting"]),
    .testTarget(name: "ConformanceTests", dependencies: ["Zero", "ZeroTesting"]),
    .testTarget(
      name: "E2ETests",
      dependencies: [
        "Zero",
        .product(name: "NIOCore", package: "swift-nio"),
        .product(name: "NIOPosix", package: "swift-nio"),
        .product(name: "NIOHTTP1", package: "swift-nio"),
        .product(name: "NIOWebSocket", package: "swift-nio"),
      ]),
  ],
  swiftLanguageModes: [.v6]
)
