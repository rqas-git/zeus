// swift-tools-version: 6.2

import PackageDescription

let package = Package(
    name: "ZeusSwift",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "zeus", targets: ["Zeus"]),
        .executable(name: "zeus-checks", targets: ["ZeusChecks"])
    ],
    targets: [
        .target(
            name: "ZeusCore",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .executableTarget(
            name: "Zeus",
            dependencies: ["ZeusCore"],
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .executableTarget(
            name: "ZeusChecks",
            dependencies: ["ZeusCore"],
            path: "Tests/ZeusChecks",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        )
    ]
)
