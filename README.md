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
cargo build -p streamit-app --release --features gst
```

Without `--features gst` the app wires a stand-in player and counter that report as down.

The `--features gst` build delay-loads the GStreamer DLLs, so it also starts on a PC without GStreamer (the player and counter then report as down). Building it still needs GStreamer's development files.

## Install and update

Each release on the [releases page](https://github.com/davojc/streamit/releases) has one program, **`streamit-windows-x64.exe`**, for every PC:

- **On the playout PC** it plays the videos on the DeckLink and sends the NDI counter. That needs GStreamer 1.24+ (runtime, **Complete**), the NDI 6 Runtime and Desktop Video installed (see [Building with GStreamer](#building-with-gstreamer-playout-pc)). StreamIt finds GStreamer on `PATH` or where its installer put it.
- **On any other PC**, for example a laptop to practise in simulation, it runs without GStreamer. The DeckLink and NDI counter show "GStreamer not installed".

StreamIt looks for GStreamer when it starts, so restart it after installing GStreamer.

To install, download it into a folder you can write to, such as `%LOCALAPPDATA%\StreamIt` (not Program Files, or updates cannot replace it), and rename it `streamit.exe` if you like. The settings live in `%APPDATA%\StreamIt`, not beside the program.

**Updating:** StreamIt checks for a new release each time it starts. When one is out, an **Update ready** button shows in the header. It opens **Settings › Updates**, where **Install update** downloads the new program, checks it, and replaces the one you are running. Then **Restart StreamIt** starts the new version. Installing and restarting are only allowed between services (stage Idle or Ended, nothing running). **Check now** asks GitHub again.

**Making a release** (developers): every push to `main` runs the Build and release workflow. It checks formatting, clippy and the tests first; only if they pass, and only when the `version` in the root `Cargo.toml` has not been released yet, does it build the program and publish it as release `v<version>`, usually within 10–15 minutes (the first build takes longer). So to release, raise the version and push to main; a push that keeps the version only runs the checks. The compiled crates are cached between runs.

## Run StreamIt

```
cargo run -p streamit-app --release                  # any PC (DeckLink and NDI show as down)
cargo run -p streamit-app --release --features gst   # the playout PC
```

The binary is `target\release\streamit.exe`; with no arguments it opens the window. **Close Bitfocus Companion first** (it holds the Stream Deck open).

Startup never changes a device: StreamIt connects to each device and listens, reads the state, checks the YouTube broadcast, then opens the window and lights the Stream Deck. A device that is unreachable shows red and is retried in the background; every device's health is polled every 2 s.

- **Service tab**: the four steps along the top (Prepare, Countdown, Live, End service) and one screen for each. Before the service it has the big **Prepare** button (or **Recover** first, with Prepare disabled, when a restart found the equipment left mid-video), the YouTube checks (bound to our stream key, Auto Start off, Auto Stop on, monitor stream on, with a **Fix** button on each failed check) and the device status. During the countdown, **Start service now** goes public and cuts to the camera in one go. It does not wait for YouTube, and if going public fails you get a **Retry going public** button. While live, each video has a **Play** button, and **Stop video** and **Unmute mics** appear while one plays. The latest warning and a **Recover** button show when StreamIt can put something right.
- **MORE menu**: in the window, it holds Start service now, Go public now, End broadcast and End service. Use it when the Stream Deck is down. On the Stream Deck, GO LIVE and GO PUBLIC are still two keys; GO LIVE does the same as Start service now.
- **Routing tab**: what the VideoHub feeds, in three sub-tabs: **MultiView** (the MultiView 4's four views), **Stream ATEM** and **Screens ATEM** (each ATEM's eight inputs). StreamIt controls none of them directly for this: each destination is fed by a VideoHub output, so changing it is a VideoHub route. Set the outputs on the **Config** sub-tab (a destination plugged straight in is "Not from the router" and can't be dropped on; Stream ATEM input 8 is always the router output to input 8, the one StreamIt plays videos through, set in Settings › Inputs & routing). A change there applies and is saved straight away. The VideoHub's inputs run down both sides, named with the VideoHub's own labels; the destinations sit in the middle, each showing the input on it now (an input on a destination also carries its badge: TL/TR/BL/BR, S1–S8 or H1–H8). On the ATEM sub-tabs the input on program is outlined red (ON AIR) and the one on preview tagged green. Drag an input onto a destination (or click an input, then a destination) to route it straight away, during a service too; if that changes an ATEM input on program, StreamIt asks first (**Keep it** / **Change it**). Stream ATEM input 8 is locked while StreamIt runs a sequence (Prepare, the countdown, a video, Go Live, End Service, Recover). Under the destinations are eight presets per sub-tab: **Save preset**, pick a slot and name it to keep the inputs (saving onto a used slot replaces it); click a preset to put them back (only what differs is routed, and a preset that changes the input on program asks first); **×** clears a preset and its name after asking, and leaves the routing as it is. The preset on the destinations now is outlined green. Presets are saved in the settings file straight away. The **VideoHub** sub-tab renames the VideoHub's inputs and outputs: change a name and press Enter (or click away) and it goes to the VideoHub at once (Escape puts it back); each output shows what it feeds. The names are the VideoHub's own, so its front panel and Videohub Setup show them too.
- **Stream Deck pages**: the deck starts on **Home**: every device's health across the top, the stage top right, and **STREAM**, **ROUTING** and **VIDEOS** in the middle (VIDEOS turns red while a video plays). Each page has **HOME** top left and the stage top right, and the deck only changes page when you press one of these keys. **Stream** has the service keys (PREPARE, COUNTDOWN, STOP COUNT, GO PUBLIC, GO LIVE, END SERVICE, END BCAST, RECOVER) on row 2. **Videos** has the 14 cue keys on rows 2 and 3, STOP VIDEO and UNMUTE MICS on the right. **Routing** has **MULTI VIEW**, **STREAM ATEM** and **SCREENS ATEM**; each of those has ROUTING (back) next to HOME, its eight presets on row 2 (green = on the destinations now) and, below, what each destination shows: the MultiView's 2×2, or the ATEM's eight inputs (red on program, green on preview, dim when not fed by the router, LOCKED for input 8 during a sequence). A preset that would change the input on program blinks **PRESS AGAIN**: press it again within 3 s to recall it.
- **Videos tab**: the countdown video (**Change…**) and the cue list (**Add videos…**, name, where it plays, level, optional return input, move up, move down, remove; at most 14). The cue list cannot be edited while a video plays. While a video plays (countdown or roll-in) the NDI counter shows the time left as `M:SS`.
- **Settings tab**: Blackmagic devices found on the network (mDNS) with a role dropdown each, plus **Search again** and **Add by IP**; every config field; the YouTube OAuth client id and secret, **Sign in** (opens the browser), and the stream key picker (**Load stream keys**). **Save** writes `%APPDATA%\StreamIt\config.toml`. Closing the window with unsaved changes asks whether to save them. **Sign in** and picking a stream key save the YouTube client and stream key straight away; **Sign in** always shows Google's account chooser, where you pick the brand account's channel if the stream runs on one, and the card then shows which channel is signed in. The mics' live levels (captured by Prepare and by each video) are kept in `levels.toml` next to it, so after a restart Go Live and Recover fade the mics up to them rather than to 0 dB. Timings, inputs and audio settings apply at once. **Device addresses and roles, the YouTube client and stream key, and the playout/NDI settings apply when you restart StreamIt.**

The window opens no console. Logs are written daily to `%APPDATA%\StreamIt\logs\streamit.<date>.log` (every command, device health change, device error and engine warning; never tokens or the client secret). Set `RUST_LOG=debug` for more detail.

### Practise with simulation

Simulation runs the real StreamIt against pretend equipment and a pretend YouTube, so you can practise a whole service with nothing connected. It never touches the real kit or YouTube, even if the network is up.

- **Switch it on:** Settings, then Equipment, then the Simulation card, then **Switch to simulation**. **Leave simulation** is on the same card. The button only works when no service is running (the stage is Idle or Ended). Otherwise it is greyed out and says "Finish or end the service first". If StreamIt is still checking the equipment, it says "Still checking the equipment — try again in a moment".
- **You cannot miss it:** every screen has a striped amber banner ("SIMULATION — nothing here is live. No equipment or YouTube is touched."). Clicking it takes you to Settings › Equipment, where the Simulation card is. The window title ends "— SIMULATION", and the Stream Deck phase key shows SIM under the stage. The Stream Deck still works, so you practise on the real keys.
- **Always starts real:** StreamIt starts in real mode every time, even if you closed it while simulating. Switching back to real does the normal startup, which only reads the equipment.
- **What is pretend** (at 1×):
  - An ATEM mix takes 1 s. Until it finishes, the ATEM still reports the old source.
  - A VideoHub route takes effect after 1 s.
  - The Web Presenter goes from Connecting to Streaming after 3 s.
  - YouTube health becomes Good 5 s after streaming starts. Going testing, live or complete takes 2 to 3 s.
  - A video plays for the real length of its mp4 or mov file, or 30 s if the length cannot be read.
  - The NDI counter is not sent anywhere. Its value shows on the Simulation tab.
  - Router numbers you have not set (output feeding ATEM input 8, DeckLink router input, fallback normal input) are filled in as 11, 39 and 13 for the simulation only. They are not saved.
- **Your settings are used:** inputs, timings and the cue list come from your real settings file, so practice matches the real set-up. **Save** still writes the real settings; the simulation picks the changes up the next time you open it. **Sign in** and **Load stream keys** are off in simulation.
- **Simulation tab** (only in simulation): you play the vision mixer for the Stream ATEM (program and preview buttons, **Cut**, **Auto**, mic level, input 8 audio), and see the other pretend equipment and the pretend YouTube.
- **Speed:** 1×, 4× or 10×. It speeds up the pretend equipment, video playback, and the VideoHub settle, closing hold and mic fade times. The real service always runs at 1×.
- **Make something go wrong:** each button breaks one thing, and pressing it again puts it right. They are Unplug VideoHub, Unplug Stream ATEM, Unplug Web Presenter, YouTube refuses public, YouTube very slow (calls take 20 s), Next video won’t load and Mic fade fails. **Crash & restart app** restarts StreamIt's engine against the pretend equipment as it stands, so you can practise **Recover**.
- **Reset simulation** puts every pretend device back to how it was before the service.
- **Separate files:** the pretend mic levels are kept in `sim-levels.toml` next to `config.toml`, so they never change your real `levels.toml`. Simulation logs go to `%APPDATA%\StreamIt\logs\sim-streamit.<date>.log`, not the real `streamit.<date>.log`.

`docs/rehearsal.md` Part 0 has a recovery drill for each fault button.

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
   cargo build -p streamit-app --release --features gst
   ```

## Probe your devices

Every probe is also built into the app, so on the playout PC you can run them without cargo. **Run probes from a terminal** (Command Prompt or PowerShell). `streamit.exe` is a windowed app with no console of its own (so a stray click or a closed console window can never stall or kill StreamIt during a service); a probe writes its output to the terminal it was started from. The prompt may come back before the probe has finished; to keep it until the probe ends, pipe the output, e.g. `streamit probe atem 192.168.4.123 state | more` in Command Prompt or `... | Out-Host` in PowerShell. Started any other way (a shortcut), a probe opens a console window of its own.

```
streamit probe discover [seconds]
streamit probe videohub <ip> dump | route <output> <input>
streamit probe webpresenter <ip> dump | status | start | stop
streamit probe atem <ip> state | preview <in> | program <in> | auto | still <player> <index> | level <in> <source> <db>
streamit probe youtube signin | upcoming | streams <client_id> <client_secret>
streamit probe youtube check <client_id> <client_secret> <broadcast_id> [stream_id]
streamit probe youtube health <client_id> <client_secret> <stream_id>
streamit probe player <file.mp4> <gain_db>     (gst build only)
streamit probe counter <seconds>               (gst build only)
streamit probe deck
```

The `cargo run ... --example` commands below run the same code.

### Discovery

```
streamit probe discover 5
```

Lists every `_blackmagic._tcp` device that answers within 5 s, with its address and every TXT record, and the kind StreamIt reads from TXT `class`. Check that the Stream ATEM, Screens ATEM, VideoHub and Web Presenter all appear and each shows a kind. `class=AtemSwitcher` and `class=Videohub` come from the Companion modules; the Web Presenter's class (`WebPresenter` or `Streaming` is assumed) and the model key (`model`, else `name`) are unconfirmed, so note what the Web Presenter actually reports. If it shows `kind None`, assign its role by hand (or with Add by IP) and report the TXT lines.

### VideoHub

Prints the raw dump the VideoHub sends on connect, then its routes (numbered from 1, as on the front panel):

```
cargo run -p streamit-videohub --example videohub-probe -- 192.168.4.100 dump
```

Save the raw dump over `crates/streamit-videohub/tests/fixtures/videohub_dump.txt` so the tests parse a real capture.

To test a route (this really switches the router, so only do it when nothing live depends on that output):

```
cargo run -p streamit-videohub --example videohub-probe -- 192.168.4.100 route <output> <input>
```

### Web Presenter 4K

Prints the raw state the Web Presenter sends on connect (control port 9977), then the stream status it parsed:

```
cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 dump
```

Save the raw dump over `crates/streamit-webpresenter/tests/fixtures/webpresenter_dump.txt` so the tests parse a real capture. If the block names differ from the ones the client expects (`STREAM STATE:` with `Status:` / `Action:` lines), fix the parser and the tests to match the capture.

To read the status, then start and stop streaming (this really starts the encoder, so set the YouTube key first and keep the broadcast non-public):

```
cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 status
cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 start
cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 stop
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

Needs the GStreamer build above. The probe uses the default config: DeckLink device 0 in mode `1080p2997`, and NDI source `StreamIt Countdown`. Make sure nothing live depends on input 8 first: the DeckLink feeds input 8 of both ATEMs. Only the Stream ATEM's input 8 is routed by the VideoHub (it is shared with Camera 8); the Screens ATEM's DeckLink feed is fixed, and StreamIt never routes it (spec call 4).

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

On any NDI receiver, add the NDI source `StreamIt Countdown`: it should show a large yellow `2:05`, centred on black, for 10 s, then count down once a second to `0:00`, then clear to plain black.

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
cargo run -p streamit-youtube --example youtube-probe -- streams <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- check <client_id> <client_secret> <broadcast_id> <stream_id>
cargo run -p streamit-youtube --example youtube-probe -- health <client_id> <client_secret> <stream_id>
```

`signin` opens your browser (or prints the address) and finishes when you approve. `upcoming` lists your upcoming broadcasts in start order with their bound stream; `streams` lists your stream keys (id, name, last four characters of the key; pick the one the Web Presenter uses); `check` prints whether the broadcast is bound to the given stream and its Auto Start / Auto Stop / monitor flags; `health` prints the ingest health of a stream key's stream.

### Stream Deck XL

**Close Bitfocus Companion first** (and the Elgato Stream Deck app): only one program can hold the deck open, and the probe will report that it cannot open the device if another has it.

```
cargo run -p streamit-deck --example deck-probe
```

The probe draws a demo snapshot on the first Stream Deck XL it finds (it keeps looking every 2 s, so you can plug it in afterwards) and prints the command each key press would send. It starts on Home; STREAM, ROUTING and VIDEOS open their pages and HOME (top left) comes back. Check that:

- the layout is 8 columns by 4 rows with the first key at the top left, and the text is the right way up and not mirrored;
- on Home, row 1 reads STREAM ATEM, SCREENS ATEM (green), VIDEO HUB (amber), WEB PRESENTER (red), YOUTUBE (green), DECKLINK and NDI (dim, unknown), then `T-0:42` in amber, and VIDEOS is red (a video plays in the demo);
- on Stream, row 2 has GO PUBLIC and GO LIVE lit, RECOVER dim; pressing a dim key prints nothing;
- on Videos, row 2 shows the cue names word-wrapped (blue = cued, red = playing, grey = played, a red key with blinking text = error), the first cue (flashing in the demo) blinks at 2 Hz, STOP VIDEO is lit (end of row 2) and UNMUTE MICS is lit (end of row 3);
- unplugging the deck and plugging it back in redraws the keys within a few seconds.

Presses print as `PlayCue(0)`, `Prepare`, and so on. Status keys and page keys print nothing.
