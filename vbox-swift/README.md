# vbox-swift

SwiftUI shell for the vbox macOS library app.

Version: `0.1.0`

The Rust CLI still builds the shipped app bundle, but the Swift source now
lives here instead of inside `crates/vbox-cli/src`.

Active source:

- `Sources/VBoxLibrary/VBoxLibrary.swift`

Version source:

- `VERSION`
- `VBoxSwiftVersion.current`

The CLI includes this file at compile time from:

- `/Users/pista/Parallels/vbox/crates/vbox-cli/src/library_ui.rs`

Standalone syntax check:

```sh
swiftc -parse-as-library Sources/VBoxLibrary/VBoxLibrary.swift -o /tmp/VBoxLibrary-check
```

SwiftPM check:

```sh
swift build
```
