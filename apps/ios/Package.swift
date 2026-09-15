// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "OctetCompanion",
    platforms: [.iOS(.v16)],
    products: [.library(name: "OctetCompanion", targets: ["OctetCompanion"])],
    dependencies: [.package(path: "../apple-shared")],
    targets: [
        .target(
            name: "OctetCompanion",
            dependencies: [.product(name: "OctetServeClient", package: "apple-shared")],
            path: "Sources/OctetCompanion",
            exclude: ["OctetCompanionApp.swift"]
        ),
        .testTarget(
            name: "OctetCompanionTests",
            dependencies: ["OctetCompanion"],
            path: "Tests/OctetCompanionTests"
        )
    ]
)
