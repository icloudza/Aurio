// swift-tools-version:5.9
import PackageDescription

// Rust 静态库由 scripts/build-mac.sh（或 build-mac-app.sh）先编译到 <仓库>/target/release
let rustLibDir = Context.packageDirectory + "/../../target/release"

let package = Package(
    name: "AurioMac",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "Aurio", targets: ["Aurio"]),
        .executable(name: "aurio-rx", targets: ["aurio-rx"]),
    ],
    targets: [
        // Rust 核心库的 C 接口
        .target(
            name: "CAurio",
            path: "Sources/CAurio",
            linkerSettings: [
                .unsafeFlags(["-L", rustLibDir]),
                .linkedLibrary("aurio_ffi"),
            ]
        ),
        // CoreAudio 设备、输入输出、接收引擎（GUI 和命令行共用）
        .target(
            name: "AurioKit",
            dependencies: ["CAurio"],
            path: "Sources/AurioKit",
            linkerSettings: [
                .linkedFramework("CoreAudio"),
                .linkedFramework("AudioToolbox"),
                .linkedFramework("AVFoundation"),
            ]
        ),
        // 命令行版
        .executableTarget(
            name: "aurio-rx",
            dependencies: ["AurioKit", "CAurio"],
            path: "Sources/aurio-rx"
        ),
        // 菜单栏 App
        .executableTarget(
            name: "Aurio",
            dependencies: ["AurioKit"],
            path: "Sources/Aurio",
            linkerSettings: [
                .linkedFramework("SwiftUI"),
                .linkedFramework("ServiceManagement"),
            ]
        ),
    ]
)
