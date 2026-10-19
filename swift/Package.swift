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
  targets: [
    .target(name: "Zero"),
    .target(name: "ZeroUI", dependencies: ["Zero"]),
    .target(name: "ZeroTesting", dependencies: ["Zero"]),
    .testTarget(name: "ZeroTests", dependencies: ["Zero", "ZeroTesting"]),
    .testTarget(name: "ConformanceTests", dependencies: ["Zero", "ZeroTesting"]),
  ],
  swiftLanguageModes: [.v6]
)
