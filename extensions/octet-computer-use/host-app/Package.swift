// swift-tools-version:6.0
import PackageDescription

// A plain executable target: the host is a single AppKit app with no
// dependencies, so there is nothing for SwiftPM to resolve and the build stays
// reproducible offline.
let package = Package(
    name: "OctetComputerUseHost",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(
            name: "OctetComputerUseHost",
            path: "Sources/OctetComputerUseHost"
        )
    ]
)
