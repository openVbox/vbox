// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "vbox-swift",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "VBoxLibrary", targets: ["VBoxLibrary"])
    ],
    targets: [
        .executableTarget(
            name: "VBoxLibrary",
            path: "Sources/VBoxLibrary"
        )
    ]
)
