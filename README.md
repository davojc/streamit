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

### Building with GStreamer (playout PC)

1. Install GStreamer 1.24+ **MSVC 64-bit**, both the **runtime** and the **development** installers, choosing **Complete** in each (the default "Typical" leaves out the `decklink` and `ndi` plugins).
2. Install the **NDI 6 Runtime** (the `ndi` plugin loads it at run time) and **Blackmagic Desktop Video** (the DeckLink driver). Check in Desktop Video Setup that the DeckLink SDI 4K output is set to 1080p29.97.
3. In the shell you build from, with the default install path:

   ```
   set PATH=C:\Program Files\gstreamer\1.0\msvc_x86_64\bin;%PATH%
   set PKG_CONFIG_PATH=C:\Program Files\gstreamer\1.0\msvc_x86_64\lib\pkgconfig
   ```

   The build needs a `pkg-config.exe` on `PATH` (GStreamer's `bin` has one in recent installers; otherwise `choco install pkgconfiglite`). The GStreamer `bin` folder must also be on `PATH` when the app runs.
4. Check the plugins are there:

   ```
   gst-inspect-1.0 decklinkvideosink
   gst-inspect-1.0 ndisink
   ```

5. Build and run the playout tests (they make a 2 s clip with `x264enc`, play it into a fake sink and check it ends):

   ```
   cargo test -p streamit-playout --features gst
   cargo build --release --features gst
   ```

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

### Playout (DeckLink) and the NDI counter

Needs the GStreamer build above. The probe uses the default config: DeckLink device 0 in mode `1080p2997`, and NDI source `StreamIt Countdown`. Make sure nothing live depends on input 8 first (the VideoHub routes the DeckLink to input 8 of both ATEMs).

```
cargo run -p streamit-playout --features gst --example playout-probe -- play <file.mp4> -3
```

The probe loads the file and holds it paused for 5 s, plays it at −3 dB, prints the position every second, then stops and outputs black for 5 s. Check on the ATEM multiview that:

- the first frame of the clip holds while it is loaded (before "playing");
- the clip plays smoothly at 29.97 (no stutter or dropped frames);
- the audio arrives on Fairlight input 8 of the Stream ATEM, on input 8 of the Screens ATEM, and on the Dante desk;
- after "stopped" the DeckLink input is black and silent.

A file with no audio track plays with silence, so the DeckLink audio stays clocked.

```
cargo run -p streamit-playout --features gst --example playout-probe -- counter 125
```

In ProPresenter, add the NDI source `StreamIt Countdown`: it should show a large yellow `2:05`, centred on black, for 10 s, then count down once a second to `0:00`, then clear to plain black.

### YouTube

StreamIt signs in to YouTube with OAuth and keeps the refresh token in the Windows Credential Manager (service `StreamIt`, entry `youtube`). You need your own OAuth client, created once:

1. Go to <https://console.cloud.google.com/> and create a project (for example "StreamIt").
2. APIs & Services > Library: enable **YouTube Data API v3**.
3. APIs & Services > OAuth consent screen: choose **External**, fill in the app name and your email, and add the scope `https://www.googleapis.com/auth/youtube`.
4. **Publish the consent screen** (set "Publishing status" to "In production"). While it is in "Testing", Google expires the refresh token after 7 days and you would have to sign in again every week. Publishing an unverified app only shows a "Google hasn't verified this app" warning at sign-in (Advanced > Go to StreamIt); that is fine for a personal client.
5. APIs & Services > Credentials > Create credentials > OAuth client ID, application type **Desktop app**. Copy the client id and client secret. (Google documents a Desktop client's secret as not confidential, so it is stored in the app config.) Enter both in Settings, or in `youtube.client_id` / `youtube.client_secret` in the config file.

Quota: the default 10,000 units/day is plenty for a weekly service.

Probe (read-only, there is deliberately no way to change a broadcast from it). Use a private test broadcast:

```
cargo run -p streamit-youtube --example youtube-probe -- signin <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- upcoming <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- check <client_id> <client_secret> <broadcast_id> <stream_id>
cargo run -p streamit-youtube --example youtube-probe -- health <client_id> <client_secret> <stream_id>
```

`signin` opens your browser (or prints the address) and finishes when you approve. `upcoming` lists your upcoming broadcasts in start order with their bound stream; `check` prints whether the broadcast is bound to the given stream and its Auto Start / Auto Stop / monitor flags; `health` prints the ingest health of a stream key's stream.
