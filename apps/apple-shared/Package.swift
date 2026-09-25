// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OctetServe",
    platforms: [
        .iOS(.v16),
        .macOS(.v13),
        .tvOS(.v16),
        .watchOS(.v9)
    ],
    products: [
        .library(name: "OctetServe", targets: ["OctetServe"])
    ],
    targets: [
        .target(
            name: "OctetServe",
            path: "Sources/OctetServe"
        )
    ]
)
