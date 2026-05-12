// swift-tools-version: 6.2

import Foundation
import PackageDescription

let developerDir = ProcessInfo.processInfo.environment["DEVELOPER_DIR"]
    ?? "/Library/Developer/CommandLineTools"
let developerFrameworks = "\(developerDir)/Library/Developer/Frameworks"
let developerLibraries = "\(developerDir)/Library/Developer/usr/lib"

let package = Package(
    name: "Zeus",
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
            path: "zeus-swift/Sources/ZeusCore",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .executableTarget(
            name: "Zeus",
            dependencies: ["ZeusCore"],
            path: "zeus-swift/Sources/Zeus",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .target(
            name: "ZeusCheckSuite",
            dependencies: ["ZeusCore"],
            path: "zeus-swift/Tests/ZeusCheckSuite",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        ),
        .testTarget(
            name: "ZeusTests",
            dependencies: ["ZeusCheckSuite"],
            path: "zeus-swift/Tests/ZeusTests",
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
            path: "zeus-swift/Tests/ZeusChecks",
            swiftSettings: [
                .swiftLanguageMode(.v5)
            ]
        )
    ]
)
