# StreamIt

Runs the church YouTube live stream from a Stream Deck. A Rust workspace for Windows 11 x64.

## Prerequisites

- Rust stable 1.98, MSVC toolchain (`x86_64-pc-windows-msvc`).
- GStreamer 1.24+ MSVC 64-bit, **complete** install (includes the `decklink` plugin and the gst-plugins-rs `ndi` plugin).
- NDI 6 Runtime.
- Blackmagic Desktop Video (DeckLink driver).
- Companion must not be running on the playout PC.

Video format everywhere is 1080p29.97.

## Build

```
cargo test --workspace
```

The default build and tests need none of the external runtimes. The real playout build, which links GStreamer, uses `--features gst`:

```
cargo build --release --features gst
```

Without `--features gst` the app wires a stand-in player and counter that report as down.

## Probe your devices

_To be filled in by later tasks (device discovery and probe instructions)._
