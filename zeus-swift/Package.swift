// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "ZeusSwift",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "zeus", targets: ["Zeus"])
    ],
    targets: [
        .executableTarget(name: "Zeus")
    ]
)
