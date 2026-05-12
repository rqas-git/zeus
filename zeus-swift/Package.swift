// swift-tools-version: 6.2

import PackageDescription
import Foundation

let developerDir = ProcessInfo.processInfo.environment["DEVELOPER_DIR"]
    ?? "/Library/Developer/CommandLineTools"
let developerFrameworks = "\(developerDir)/Library/Developer/Frameworks"
let developerLibraries = "\(developerDir)/Library/Developer/usr/lib"

let package = Package(
    name: "ZeusSwift",
    platforms: [
        .macOS(.v14)
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
            resources: [
                .copy("Resources/Fonts")
            ],
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .target(
            name: "ZeusCheckSuite",
            dependencies: ["ZeusCore"],
            path: "Tests/ZeusCheckSuite",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .testTarget(
            name: "ZeusTests",
            dependencies: ["ZeusCheckSuite"],
            swiftSettings: [
                .unsafeFlags(["-F", developerFrameworks])
            ],
            linkerSettings: [
                .unsafeFlags([
                    "-F", developerFrameworks,
                    "-Xlinker", "-rpath",
                    "-Xlinker", developerFrameworks,
                    "-Xlinker", "-rpath",
                    "-Xlinker", developerLibraries
                ])
            ]
        ),
        .executableTarget(
            name: "ZeusChecks",
            dependencies: ["ZeusCheckSuite"],
            path: "Tests/ZeusChecks",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        )
    ]
)
