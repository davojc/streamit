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

- **Service tab**: the four steps along the top (Prepare, Countdown, Live, End service) and one screen for each. Before the service it has the big **Prepare** button (or **Recover** first, with Prepare disabled, when a restart found the equipment left mid-video), the YouTube checks (bound to our stream key, Auto Start off, Auto Stop on, monitor stream on, with a **Fix** button on each failed check) and the device status. During the countdown, **Start service now** goes public and cuts to the camera in one go. It does not wait for YouTube, and if going public fails you get a **Retry going public** button. While live, each video has a **Play** button; the one playing shows the time left, a progress bar and **Stop**, and the others wait. End service is the fourth step in the steps bar: while live it is a red button there (it asks first), as well as in MORE. While a service is under way the LIVE badge in the header opens the broadcast's live control room in YouTube Studio. The latest warning and a **Recover** button show when StreamIt can put something right.
- **MORE menu**: in the window, it holds Start service now, Go public now, End broadcast and End service. End service, here or on the Live screen, asks first ("End the service?": **Keep going** / **End service**). Use it when the Stream Deck is down. On the Stream Deck, GO LIVE and GO PUBLIC are still two keys; GO LIVE does the same as Start service now.
- **Steps**: on the Prepare, countdown and End Service screens the right-hand column lists that stage's steps as circles joined top to bottom: grey until done, amber while happening (with what it waits on, or the time left), green with the time once done, red with the reason if it failed. Prepare: stream audio levels read, opening still on air, stream audio faded down, Web Presenter streaming, YouTube receiving, YouTube test started. Countdown: hall screens to the countdown, stream to the countdown (only when it goes online), countdown video playing, YouTube public. End service: closing still on air, stream audio faded down, closing hold, YouTube broadcast ended, Web Presenter stopped. Prepare starts every step again for the next service. The YouTube checks, the device status and the videos for the service sit under the Prepare panel.
- **Status panels**: from Prepare until the service ends, four panels run across the top of the Service screen.
  - **YouTube**: is YouTube receiving the stream on our key (green "Receiving", amber when it is poor or not receiving yet, red "NOT RECEIVING"), whether the broadcast is public, and how many are watching.
  - **Online sees**: what the Stream ATEM has on program and on preview, named by what feeds it: the VideoHub input's label, "Video: <name>" while a video plays, or "Holding still".
  - **Stream audio**: the stream audio bar (silent until live, down for a video, with **Stream audio up**), then the audio inputs you pick in Settings › Timings & audio ("Audio inputs shown on the Service screens": a name and the Fairlight input and source), each tagged ON, AFV or OFF with its fader.
  - **Hall screens**: what the Screens ATEM has on program and on preview, named the same way.
- **Live chat**: the Live screen has the broadcast's live chat down the side, newest at the bottom, with moderators' and the channel's own messages marked. Type in **Reply as the channel** and press Enter or **Send** to post as the signed-in channel (the brand account's channel if that was picked at sign-in); the reply shows in the chat once YouTube has it. **Open in YouTube** opens the chat in its own window. StreamIt reads the chat at most every 10 s, from Prepare until the service ends: 5 quota units a read (about 1,800 an hour), and about 50 for each reply. The full YouTube permission StreamIt already asks for covers the chat, so there is nothing to sign in to again. In simulation the pretend viewers start chatting once the stream is healthy.
- **Is YouTube getting the stream?** StreamIt asks YouTube every 12 s how the stream on our key looks, and reads the viewer count at the same time. The YouTube panel turns red once nothing has arrived for 20 s while live (a short gap is normal when the encoder reconnects). The red state also raises the warning bar, writes "YouTube stopped receiving the stream" in the activity log (and "receiving the stream again" when it comes back) and turns the Stream Deck YOUTUBE key red with "NO STREAM". The health check and the viewer count cost 1 unit of the YouTube API quota each (about 600 an hour together).
- **Routing tab**: what the VideoHub feeds, in three sub-tabs: **MultiView** (the MultiView 4's four views), **Stream ATEM** and **Screens ATEM** (each ATEM's eight inputs). StreamIt controls none of them directly for this: each destination is fed by a VideoHub output, so changing it is a VideoHub route. Set the outputs on the **Config** sub-tab (a destination plugged straight in is "Not from the router" and can't be dropped on; Stream ATEM input 8 is always the router output to input 8, the one StreamIt plays videos through, set in Settings › Inputs & routing). A change there applies and is saved straight away. The VideoHub's inputs run down both sides, named with the VideoHub's own labels; the destinations sit in the middle, each showing the input on it now (an input on a destination also carries its badge: TL/TR/BL/BR, S1–S8 or H1–H8). On the ATEM sub-tabs the input on program is outlined red (ON AIR) and the one on preview tagged green. Drag an input onto a destination (or click an input, then a destination) to route it straight away, during a service too; if that changes an ATEM input on program, StreamIt asks first (**Keep it** / **Change it**). Stream ATEM input 8 is locked while StreamIt runs a sequence (Prepare, the countdown, a video, Go Live, End Service, Recover). Under the destinations are eight presets per sub-tab: **Save preset**, pick a slot and name it to keep the inputs (saving onto a used slot replaces it); click a preset to put them back (only what differs is routed, and a preset that changes the input on program asks first); **×** clears a preset and its name after asking, and leaves the routing as it is. The preset on the destinations now is outlined green. Presets are saved in the settings file straight away. The **VideoHub** sub-tab renames the VideoHub's inputs and outputs: change a name and press Enter (or click away) and it goes to the VideoHub at once (Escape puts it back), and × clears a name to its number (`#5`); each output shows what it feeds. The names are the VideoHub's own, so its front panel and Videohub Setup show them too.
- **Stream Deck pages**: the deck shows the page the window is on, and picking a page on the deck opens it in the window too (Settings and Simulation leave the deck on Service). The top row is the same on every page: **SERVICE**, **VIDEOS**, **SCENES** and **ROUTING** (the one shown is lit; VIDEOS turns red while a video plays), then on Routing **MULTI VIEW**, **STREAM ATEM** and **SCREENS ATEM**, and the stage top right (with the countdown or the time on air, and SIM in simulation). There are no device-status keys: the equipment bar in the window shows them. **Service** follows the stage: before the service PREPARE and START SERVICE NOW (which asks first: CONFIRM START blinks below it for 5 s), with RECOVER and END BCAST when offered; once prepared PREPARE AGAIN, START COUNTDOWN, START SERVICE NOW and ABANDON; during the countdown START SERVICE NOW, STOP COUNTDOWN, GO PUBLIC NOW and ABANDON (ABANDON asks first: CONFIRM ABANDON blinks below it for 5 s; STOP COUNTDOWN and ABANDON go once YouTube is going public); while live the 14 videos (rows 2 and 3), STOP VIDEO, STREAM AUDIO UP, five scenes along the bottom, a key for whatever is offered (Recover, Go live, Retry public, End broadcast) and END SERVICE, which asks first (CONFIRM END blinks beside it for 5 s; any other key, or waiting, cancels); while ending SKIP HOLD and END BCAST. **Videos** has the 14 video keys, STOP VIDEO and STREAM AUDIO UP; before the countdown a video key test-plays it (hall and input 8, never online), from the countdown on it plays it. **Scenes** has a key for each scene (amber = the scene on now). **Routing** has the target's eight presets on row 2 (green = on the destinations now) and, below, what each destination shows: the MultiView's 2×2, or the ATEM's eight inputs (red on program, green on preview, dim when not fed by the router, LOCKED for input 8 during a sequence). A preset that would change the input on program blinks **PRESS AGAIN**: press it again within 3 s to recall it.
- **Scenes tab**: a scene puts a Screens ATEM input on the hall screens (a cut; or leaves them as they are) and fires an ETC Eos lighting cue (list and cue; or no lighting change). Up to 12, in order, with **Add scene**, move up and down, **GO** on each, and the selected one's name, Stream Deck key name, hall screens input (named by what the VideoHub feeds it) and lighting cue on the right; **Remove** asks for a second click. Changes save straight away, like the Routing presets, and scenes are kept in the profile. The Live screen shows the scenes in a row between the status panels and the videos, one press each, with the scene on now lit. A video can name a **scene while it plays**: StreamIt goes to it when the video starts and back to the scene before when the video ends or is stopped (unless another scene was picked meanwhile). While a video or the countdown is on the hall screens, a scene leaves the hall on it and only fires its lighting cue.
- **Abandon**: once prepared, and during the countdown, **Abandon** (click twice) goes back to the start: the countdown stops as with Stop countdown, the Web Presenter stops and the steps clear. YouTube is left as it is, still private, so Prepare can use the same broadcast again. Once YouTube is going public (the countdown's go-public or Go public now) there is no abandoning or stopping the countdown: the service goes on, and End Service ends it.
- **Countdown online**: by default the countdown video plays to the hall only and online sees the opening still, in silence. Tick **The countdown video goes online too** in Settings › Playout & NDI counter and it also goes to the stream: the VideoHub routes the DeckLink to Stream ATEM input 8 (as for a video), input 8's sound comes on and the stream cuts to it. At 0:00 the stream cuts to the camera as usual, then input 8's sound goes off and its route goes back. Stop countdown puts the stream back on the opening still. Without the router numbers set it plays to the hall only, with a warning.
- **Videos tab**: the countdown video (**Change…**) and the cue list (**Add videos…**, name, where it plays, level, optional return input, the scene while it plays, move up, move down, remove; at most 14). The cue list cannot be edited while a video plays. While a video plays (countdown or roll-in) the NDI counter shows the time left as `M:SS`. Each video has a **Test** button to check it plays on the DeckLink: a test play cuts the hall screens to the Screens ATEM's DeckLink input and routes the DeckLink to Stream ATEM input 8 through the VideoHub, but never puts input 8 on program or turns its audio on, so nothing goes online (watch input 8 on the multiview and its audio meter). The row shows the time left and **Stop**, and a panel says where to watch it. When it ends or is stopped the hall goes back to what it showed and input 8 to its route before. Test play is off from the countdown until the service ends, while another sequence runs, while the Stream ATEM has input 8 on program, and until the router numbers are set in Settings › Inputs & routing. It shows in the hall, so test before people arrive.
- **Profiles** (top of the Settings tab): a profile is a saved copy of all of StreamIt's settings (everything in Settings, the Routing presets and the cue list) plus a backup of each device's own settings where StreamIt can read them, in its own folder under `%APPDATA%\StreamIt\profiles\<name>`. **Save as new…** names one; **Save into <name>** updates the one in use (shown in the header, with "Changed since it was saved or loaded" when it differs). Saving reads every connected device: the VideoHub's input and output names and routing (`devicesideohub.txt`, in the VideoHub's own protocol blocks), the Web Presenter's streaming settings without the stream key (`devices\web-presenter.txt`), and, for reference only, what StreamIt can read of each ATEM (program, preview, stream audio levels). A device that is not connected is noted as not backed up. **Load…** replaces StreamIt's settings and asks which device backups to put back (VideoHub, Web Presenter; ticked by default); equipment addresses apply after a restart, as usual. StreamIt cannot put an ATEM's set-up back: for a full ATEM backup, save it from ATEM Software Control (File › Save) into the profile's folder, where it is kept. **Delete…** sends a profile's folder to the Recycle Bin. Profiles are off in simulation and while Settings has unsaved changes, and loading is refused during a service.
- **Lighting (ETC Eos)**: optional. In Settings › Equipment › Lighting console, give the console's address; StreamIt talks OSC over TCP on port 3032. On the console turn on OSC TCP (Setup › System › Show Control) and set its "TCP Format for OSC" to the one chosen here (OSC 1.1 SLIP by default, or OSC 1.0 packet length). StreamIt subscribes, so it shows the show name and the active cue, and pings every few seconds: the console appears with the other equipment (Lighting) once set up, red when it does not answer. The card's **Fire cue** row fires a cue by list and number (`/eos/cue/<list>/<cue>/fire`), for trying it out; Scenes will use the same connection. The address applies after a restart. The simulation has a pretend console.
- **Settings tab**: Blackmagic devices found on the network (mDNS, plus any ATEM that answers the ATEM hello on UDP 9910 at an address on this computer's own /24 networks, for ATEMs such as the Television Studio HD8 ISO that do not advertise) with a role dropdown each, plus **Search again** and **Add by IP**. The roles are Stream switcher, Screens switcher, Router, MultiView and Stream encoder; a MultiView 4 is told apart from a VideoHub by its model name. A MultiView is optional: StreamIt only checks it answers on port 9990, shown in the equipment bar; every config field; the YouTube OAuth client id and secret, **Sign in** (opens the browser), and the stream key picker (**Load stream keys**), and the **API quota** StreamIt has used today: units out of the default 10,000, the number of calls, and when it resets (midnight Pacific time). Each call costs 1 unit, reading the live chat 5 and sending a message 20; the count is kept in `youtube-quota.toml` next to the config, and the YouTube panel on the Service screen shows the units used today too. It counts only this StreamIt's calls, not other apps on the same Google project. **Save** writes `%APPDATA%\StreamIt\config.toml`. Closing the window with unsaved changes asks whether to save them. **Sign in** and picking a stream key save the YouTube client and stream key straight away; **Sign in** always shows Google's account chooser, where you pick the brand account's channel if the stream runs on one, and the card then shows which channel is signed in. The stream audio's live levels (captured by Prepare and by each video) are kept in `levels.toml` next to it, so after a restart Go Live and Recover fade the stream audio up to them rather than to 0 dB. Timings, inputs and audio settings apply at once. **Device addresses and roles, the YouTube client and stream key, and the playout/NDI settings apply when you restart StreamIt.**

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
  - YouTube health becomes Good 5 s after streaming starts; then the viewer count grows and a pretend viewer writes in the live chat every 15 s. The pretend Stream ATEM reports its audio inputs for the Stream audio panel: the stream audio sources on at their faders, the video input following video (or on), the rest following video. Going testing, live or complete takes 2 to 3 s.
  - A video plays for the real length of its mp4 or mov file, or 30 s if the length cannot be read.
  - The NDI counter is not sent anywhere. Its value shows on the Simulation tab.
  - Router numbers you have not set (output feeding ATEM input 8, DeckLink router input, fallback normal input) are filled in as 11, 39 and 13 for the simulation only. They are not saved.
- **Your settings are used:** inputs, timings and the cue list come from your real settings file, so practice matches the real set-up. **Save** still writes the real settings; the simulation picks the changes up the next time you open it. **Sign in** and **Load stream keys** are off in simulation.
- **Simulation tab** (only in simulation): you play the vision mixer for the Stream ATEM (program and preview buttons, **Cut**, **Auto**, stream audio level, input 8 audio), and see the other pretend equipment and the pretend YouTube.
- **Speed:** 1×, 4× or 10×. It speeds up the pretend equipment, video playback, and the VideoHub settle, closing hold and stream audio fade times. The real service always runs at 1×.
- **Make something go wrong:** each button breaks one thing, and pressing it again puts it right. They are Unplug VideoHub, Unplug Stream ATEM, Unplug Web Presenter, YouTube refuses public, YouTube very slow (calls take 20 s), Next video won’t load and Stream audio fade fails. **Crash & restart app** restarts StreamIt's engine against the pretend equipment as it stands, so you can practise **Recover**.
- **Reset simulation** puts every pretend device back to how it was before the service.
- **Separate files:** the pretend stream audio levels are kept in `sim-levels.toml` next to `config.toml`, so they never change your real `levels.toml`. Simulation logs go to `%APPDATA%\StreamIt\logs\sim-streamit.<date>.log`, not the real `streamit.<date>.log`.

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

Expect program and preview to match the multiview, both media players with the still each one holds, and a Fairlight line for `1001 / -65280` (the stream audio source) with its fader level. Write down that level so you can put it back. Then:

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 preview 3
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 still 1 <opening>
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 -10
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 <original>
```

- `preview 3`: input 3 should appear on the preview window.
- `still 1 <opening>`: media player 1 (input 3010) should show the opening still. `<opening>` is the still's 0-based slot in the media pool, the same number as `opening_still` in the config (ATEM Software Control numbers its slots from 1, so slot 1 there is 0 here).
- `level 1001 -65280 -10`: the stream audio fader in ATEM Software Control's Fairlight page should move to −10 dB, and nothing else on that channel should change. The `level … <original>` run puts it back.

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

The probe draws a demo snapshot (live for 42 minutes, a video playing) on the first Stream Deck XL it finds (it keeps looking every 2 s, so you can plug it in afterwards) and prints the command each key press would send. It starts on Service; the top row moves between the pages (the probe has no window, so only the deck changes). Check that:

- the layout is 8 columns by 4 rows with the first key at the top left, and the text is the right way up and not mirrored;
- the top row reads SERVICE (lit), VIDEOS (red: a video plays), SCENES, ROUTING, three blanks, then `LIVE` and the time on air in red;
- on Service, rows 2 and 3 show the cue names word-wrapped (blue = cued, red = playing, grey = played, a red key with blinking text = error), the first cue (flashing in the demo) blinks at 2 Hz, STOP VIDEO (end of row 2) and STREAM AUDIO UP (end of row 3) are lit, and the bottom row has RETRY PUBLIC and END SERVICE; END SERVICE makes CONFIRM END blink beside it;
- on Routing, the middle of the top row reads MULTI VIEW, STREAM ATEM, SCREENS ATEM and picks the target;
- unplugging the deck and plugging it back in redraws the keys within a few seconds.

Presses print as `PlayCue(0)`, `GoPublic`, and so on. Page keys print nothing.
