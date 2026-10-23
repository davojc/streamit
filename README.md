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

_More entries are added by later tasks (device discovery and the other probes)._

### VideoHub

Prints the raw dump the VideoHub sends on connect, then its routes (numbered from 1, as on the front panel):

```
cargo run -p streamit-videohub --example probe -- 192.168.4.100 dump
```

Save the raw dump over `crates/streamit-videohub/tests/fixtures/videohub_dump.txt` so the tests parse a real capture.

To test a route (this really switches the router, so only do it when nothing live depends on that output):

```
cargo run -p streamit-videohub --example probe -- 192.168.4.100 route <output> <input>
```

### Web Presenter 4K

Prints the raw state the Web Presenter sends on connect (control port 9977), then the stream status it parsed:

```
cargo run -p streamit-webpresenter --example probe -- 192.168.4.93 dump
```

Save the raw dump over `crates/streamit-webpresenter/tests/fixtures/webpresenter_dump.txt` so the tests parse a real capture. If the block names differ from the ones the client expects (`STREAM STATE:` with `Status:` / `Action:` lines), fix the parser and the tests to match the capture.

To read the status, then start and stop streaming (this really starts the encoder, so set the YouTube key first and keep the broadcast non-public):

```
cargo run -p streamit-webpresenter --example probe -- 192.168.4.93 status
cargo run -p streamit-webpresenter --example probe -- 192.168.4.93 start
cargo run -p streamit-webpresenter --example probe -- 192.168.4.93 stop
```

### ATEM switchers

The probe connects over UDP 9910, waits for the switcher's full state, does one thing, then prints program, preview, the media players and every Fairlight source's fader level and mix option. Every command it sends is confirmed by the switcher (acked) before it reports success. Check each step on the switcher's multiview and in ATEM Software Control.

**Stream ATEM** (Television Studio HD8 ISO, Fairlight audio). These change the live picture and sound, so only do them when nothing is streaming.

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 state
```

Expect program and preview to match the multiview, both media players with the still each one holds, and a Fairlight line for `1001 / -65280` (the mic/hall source) with its fader level. Write down that level so you can put it back. Then:

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 preview 3
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 still 1 <opening>
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 -10
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 <original>
```

- `preview 3`: input 3 should appear on the preview window.
- `still 1 <opening>`: media player 1 (input 3010) should show the opening still. `<opening>` is the still's 0-based slot in the media pool, the same number as `opening_still` in the config (ATEM Software Control numbers its slots from 1, so slot 1 there is 0 here).
- `level 1001 -65280 -10`: the mic fader in ATEM Software Control's Fairlight page should move to −10 dB, and nothing else on that channel should change. The `level … <original>` run puts it back.

**Screens ATEM** (Television Studio HD, classic audio). Only switching is used here, so the probe should report no Fairlight sources.

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 state
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 preview 8
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 auto
```

`preview 8` should put the DeckLink input on preview; `auto` should run the transition, print `transition done` when it finishes, and leave input 8 on program on the hall screens.

If a command reports `timeout`, the switcher never confirmed it; if `state` reports `disconnected`, the probe never got the switcher's state (check the address, and that it answers in ATEM Software Control).
