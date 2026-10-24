# StreamIt on-kit rehearsal

Run this on the real kit, against a **private test broadcast**. Never run it on a real service broadcast.

Do Part 0 at the desk any time. Do Part A once. Do Part B in order. Write the results in Part C.

## Part 0. Practise in simulation

Do this first, at the desk, with no kit. Nothing real is touched and no broadcast is needed. Part B is the real rehearsal.

1. Open StreamIt. Go to Settings, then Equipment, then the Simulation card. Press **Switch to simulation**. The amber banner appears and a Simulation tab is added.
2. On the Simulation tab, set the speed. 4× is good for drills. Press **Reset simulation** between drills.
3. Run to the step the drill needs: **Prepare**, then the countdown, then live, then play a video from the Service tab.
4. When you finish, press **Leave simulation** on the Simulation card. It works only when the stage is Idle or Ended.

If the Stream Deck is down, use the **MORE** menu in the window (Start service now, Go public now, End broadcast, End service).

Drills. Each fault button breaks one thing. Press it again to put it right.

- **VideoHub unplugged mid-video.** Play a video. Press **Unplug VideoHub**. Let the video end, or press **Stop video**. A warning appears. Press **Unplug VideoHub** again to plug it back in, then press **Recover**. Pass if the warning clears and Camera 8 is back on input 8.
- **YouTube refuses public.** Press **YouTube refuses public**. Start the countdown, or press **Start service now**. The picture still cuts on time. A warning appears with **Retry going public**. Press **YouTube refuses public** again to let it through, then press **Retry going public**. Pass if the broadcast shows as public.
- **Mic fade fails at T-0.** Press **Mic fade fails**. Start the countdown and let it reach T-0. A warning appears. Press **Mic fade fails** again to fix it, then press **Recover**. Pass if the mics come up.
- **Crash & restart mid-video.** Play a video. Press **Crash & restart app**. StreamIt restarts against the same pretend equipment and changes nothing by itself. Press **Recover**. Pass if the right input is on air and the mics are right.

## Part A. Before the rehearsal

### A1. Install and build

1. On the playout PC, install GStreamer 1.24+ MSVC 64-bit. Run both the runtime and the development installers. Choose **Complete** each time.
2. Install the NDI 6 Runtime.
3. Install Blackmagic Desktop Video. In Desktop Video Setup, set the DeckLink SDI 4K output to 1080p29.97.
4. Open a shell and follow "Building with GStreamer" in `README.md` (the PATH and PKG_CONFIG_PATH lines).
5. Check the plugins exist:
   ```
   gst-inspect-1.0 decklinkvideosink
   gst-inspect-1.0 ndisink
   ```
   Both must print details. If not, the install was not Complete.
6. Build with GStreamer:
   ```
   cargo test -p streamit-playout --features gst
   cargo build -p streamit-app --release --features gst
   ```
   Pass if both finish with no errors. If the build fails, stop and send us the error.
7. The program is `target\release\streamit.exe`. The probes below can be run as `streamit probe ...` or with `cargo run ... --example`.

### A2. YouTube set-up

1. In <https://console.cloud.google.com/>, create a project (for example "StreamIt").
2. Enable **YouTube Data API v3**.
3. Set up the OAuth consent screen: **External**, your app name and email, scope `https://www.googleapis.com/auth/youtube`.
4. **Publish the consent screen** (status "In production"). If you leave it in "Testing", the sign-in expires after 7 days.
5. Create an OAuth client ID of type **Desktop app**. Keep the client id and secret.
6. In YouTube Studio, make a **private** test broadcast. Note its broadcast id. Bind it to the stream key the Web Presenter uses.

### A3. Close Companion

Close Bitfocus Companion and the Elgato Stream Deck app. Only one program can hold the Stream Deck open. Check that neither is in the system tray.

### A4. Device probes

Run each probe. Look at what it prints. If the real output differs from the fixture, replace the fixture with the real capture and run `cargo test --workspace`. If a test now fails, the code or the test needs fixing. Tell us.

Do not run a probe that changes something while the church is streaming.

#### mDNS discover

```
streamit probe discover 5
```

- Look for: the Stream ATEM, Screens ATEM, VideoHub and Web Presenter are all listed, and each has a kind.
- Note exactly what the Web Presenter reports for `class` and `model`. If it shows `kind None`, copy all its TXT lines to us. You will set its role by hand later.
- No fixture to replace.

#### VideoHub (192.168.4.100)

```
cargo run -p streamit-videohub --example videohub-probe -- 192.168.4.100 dump
```

- Look for: the dump prints, then the routes, numbered from 1.
- Save the raw dump over `crates/streamit-videohub/tests/fixtures/videohub_dump.txt`.
- Then run `cargo test -p streamit-videohub`. If the block-count check fails, update that number.
- Optional route test. This really switches the router. Only do it when nothing live depends on that output:
  ```
  cargo run -p streamit-videohub --example videohub-probe -- 192.168.4.100 route <output> <input>
  ```
  Put the route back afterwards.

#### Web Presenter 4K (192.168.4.93)

```
cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 dump
```

- Look for: a `STREAM STATE:` block with `Status:` and `Action:` lines. If the names differ, tell us.
- Save the raw dump over `crates/streamit-webpresenter/tests/fixtures/webpresenter_dump.txt`.
- **REDACT THE STREAM KEY before you commit.** The dump may contain your YouTube stream key. Replace the key value with `REDACTED` and search the file for the key before you commit it. Never commit a live key.
- Then run `cargo test -p streamit-webpresenter`.
- Start and stop test. This really starts the encoder. Set the YouTube key on the Web Presenter first, and keep the test broadcast private:
  ```
  cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 status
  cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 start
  cargo run -p streamit-webpresenter --example webpresenter-probe -- 192.168.4.93 stop
  ```
  Pass if the status changes to streaming after `start` and back after `stop`.

#### Stream ATEM (192.168.4.123)

Only do this when nothing is streaming. These commands change the live picture and sound.

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 state
```

- Look for: program and preview match the multiview. Both media players are listed with their stills. There is a Fairlight line for `1001 / -65280` with a fader level. **Write that level down.**
- Then (player 1 is the default):
  ```
  cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 preview 3
  cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 still 1 <opening>
  cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 -10
  cargo run -p streamit-atem --example atem-probe -- 192.168.4.123 level 1001 -65280 <original>
  ```
- Look for: input 3 shows on preview. Media player 1 (input 3010) shows the opening still. The mic fader in ATEM Software Control moves to -10 dB, nothing else on that channel changes, and the last command puts it back.
- `<opening>` is the still's slot in the media pool counting from 0. ATEM Software Control counts from 1, so slot 1 there is 0 here.
- Every command must report success. `timeout` means the switcher did not confirm. `disconnected` means no state arrived: check the address.
- Also confirm the ATEM accepts the empty-packet keep-alive ping (the probe stays connected for the whole run without dropping).
- No fixture to replace. One test in `crates/streamit-atem/src/commands.rs` has a `TODO(capture)` note for the InCm command. Leave it unless a probe step above fails.

#### Screens ATEM (192.168.4.98)

```
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 state
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 preview 8
cargo run -p streamit-atem --example atem-probe -- 192.168.4.98 auto
```

- Look for: `state` shows no Fairlight sources. `preview 8` puts the DeckLink input on preview. `auto` prints `transition done` and leaves input 8 on program on the hall screens.
- This changes the hall screens. Do it when the hall is empty.

#### YouTube

Use the private test broadcast.

```
cargo run -p streamit-youtube --example youtube-probe -- signin <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- upcoming <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- streams <client_id> <client_secret>
cargo run -p streamit-youtube --example youtube-probe -- check <client_id> <client_secret> <broadcast_id> <stream_id>
cargo run -p streamit-youtube --example youtube-probe -- health <client_id> <client_secret> <stream_id>
```

- Look for: `signin` opens the browser. Click through the "Google hasn't verified this app" warning (Advanced, Go to StreamIt). `upcoming` lists the test broadcast. `streams` lists the key the Web Presenter uses, with its last four characters. `check` shows the bound stream and the Auto Start, Auto Stop and monitor flags. `health` shows the ingest health.
- The probe cannot change a broadcast. Fixes and transitions are only tried from the app, on the private test broadcast.
- No fixture to replace.

#### Playout (DeckLink) and NDI counter

Needs the GStreamer build. Make sure nothing live depends on input 8.

```
cargo run -p streamit-playout --features gst --example playout-probe -- play <file.mp4> -3
cargo run -p streamit-playout --features gst --example playout-probe -- counter 125
```

- `play` look for: the first frame of the clip is held while the clip is loaded (before "playing"). The clip plays smoothly at 29.97 with no stutter or dropped frames. The audio arrives on Fairlight input 8 of the Stream ATEM, on input 8 of the Screens ATEM, and on the Dante desk. After "stopped" the DeckLink input is black and silent.
- `counter`: on any NDI receiver, add the NDI source `StreamIt Countdown`. Look for: a large yellow `2:05`, centred on black, for 10 s. Then it counts down once a second to `0:00`, then clears to plain black.

#### Stream Deck XL

Companion must be closed.

```
cargo run -p streamit-deck --example deck-probe
```

Look for:

- 8 columns by 4 rows, first key at top left. Text is the right way up and not mirrored.
- Row 0: STREAM ATEM, SCREENS ATEM (green), VIDEO HUB (amber), WEB PRESENTER (red), YOUTUBE (green), DECKLINK and NDI (dim), then `T-0:42` in amber.
- Row 1: GO PUBLIC and GO LIVE lit, RECOVER dim. Pressing a dim key prints nothing.
- Row 2: cue names are word-wrapped and readable. The first cue blinks at 2 Hz. STOP VIDEO is lit.
- UNMUTE MICS is lit, bottom right.
- Unplug the deck and plug it back in. The keys redraw within a few seconds.

### A5. Settings

Open StreamIt (`target\release\streamit.exe`) and go to the Settings tab.

1. **Roles.** The kit found by mDNS is listed. Give each device its role: Stream ATEM, Screens ATEM, VideoHub, Web Presenter. Use **Add by IP** for any that did not appear.
2. **YouTube.** Enter the client id and secret. Press **Sign in**. Press **Load stream keys** and pick the Web Presenter's key.
3. **VideoHub.** Set **Output feeding ATEM input 8** to the router output that goes to Stream ATEM input 8. Set **DeckLink router input** to the router input the DeckLink SDI output is plugged into. Set **Fallback normal input** to the router input Camera 8 is normally on: this is what StreamIt routes back onto input 8 after a video, and what Recover uses. 0 means "not set". With 0, Recover warns instead of restoring the route.
4. **Stream ATEM.** Set **DeckLink input** to 8. (Input 8 is shared: Camera 8 and the DeckLink both reach it through the VideoHub, which is why step 3 matters. There is no separate "Camera 8 input" setting.) Set **Opening still** and **Closing still** (media pool slots, counting from 0). They must be two different stills.
5. **Screens ATEM.** Set **DeckLink input** to 8. Set **Input at T-0** to the input the hall screens go to at T-0.
6. **Playout.** Set the countdown file, the DeckLink device, and the NDI source name `StreamIt Countdown`.
7. Press **Save**. Then **restart StreamIt**. Device addresses, roles, the YouTube client and key, and the playout and NDI settings only apply after a restart.
8. After the restart, launch on the church network and check: the discovered kit is listed, the roles stuck, and the Stream Deck shows live status for each device. Startup must not change any device. In the log (`%APPDATA%\StreamIt\logs`) there should be no commands sent during startup.
9. Add at least two short videos to the cue list on the Run tab: one for the hall and online, one with a return input.

## Part B. The rehearsal

Select the private test broadcast in the Run tab. Keep it private until check 3. Press keys on the Stream Deck or the matching buttons on the Run tab. Both do the same thing. Do the checks in order.

1. **Stage 0: the checks.**
   Look at the checks list. Turn Auto Start on in YouTube Studio for the test broadcast. Press **Fix** next to Auto Start. Auto Start must go off.
   *Pass if:* all the checks show (stream key, Auto Start off, Auto Stop on, monitor stream on). The Auto Start check fails when you turn it on, and **Fix** turns it off again. Nothing else changes by itself.

2. **Prepare.**
   Press PREPARE (Stream Deck key or Run tab button).
   *Pass if:* the opening still is on air and silent. The Web Presenter is streaming. The stage reaches READY within 60 s. The broadcast is in `testing` in YouTube Studio.

3. **Countdown.**
   Press COUNT DOWN. Watch the hall screen, your NDI receiver, and YouTube Studio.
   *Pass if:* the hall shows the countdown. The NDI counter is yellow on the NDI receiver. The broadcast goes public at T-10s. **Measure the go-public latency:** the time in seconds from the moment the countdown reaches T-10s to the moment YouTube Studio shows the broadcast as Public. Write it in Part C.

4. **T-0.**
   Before T-0, put Camera 5 on the Stream ATEM preview. Let the countdown reach T-0.
   *Pass if:* Camera 1 goes on air (not Camera 5). The mics fade up. The Screens ATEM goes to its T-0 input.

5. **One roll-in per destination.**
   Play one video to the hall and online, and one to each other destination in your cue list. Use the video with a return input at least once.
   *Pass if:*
   - You see no black on the stream multiview during the route.
   - Camera 8 is back on input 8 afterwards (the VideoHub route is back on the Fallback normal input).
   - If an operator changes the preview during the video, the change is honoured and not overridden.
   - After a video the ATEM returns to preview, unless the video has a return input.
   - A second press of a cue while one video is playing is ignored and its key flashes.

6. **Unmute Mics during a video.**
   While a video plays, press UNMUTE MICS.
   *Pass if:* the mics come back up during the video. The key goes dim.

7. **Stop Video.**
   While a video plays, press STOP VIDEO.
   *Pass if:* the video stops, the DeckLink goes black and silent, and the picture returns to the right input.

8. **End Service.**
   Press END SERVICE.
   *Pass if:* the closing still shows. The broadcast ends after 10 s. The Web Presenter stops streaming. The recording starts and ends on the stills (opening still at the start, closing still at the end).

9. **Crash test.**
   Start another run through to a video playing. Kill the app (End task in Task Manager) while the video plays. Restart StreamIt.
   *Pass if:* nothing changes on its own after the restart. Press RECOVER. It fixes the state (the right input on air, mics correct).

10. **VideoHub cable.**
    Play a video to the hall and online. While it plays, pull the VideoHub's network cable. Let the video end (or press STOP VIDEO).
    *Pass if:* the VIDEO HUB key goes red while the video plays. RECOVER stays dim while the video is still playing; it lights when the video ends and its return path cannot put the VideoHub route back (the cue shows an error). Plug the cable back in, press RECOVER, and the state is right again (Camera 8 back on input 8).

11. **Crash during the countdown.**
    Start the countdown. Kill the app mid-countdown. Restart StreamIt.
    *Pass if:* the stage shows READY (or LIVE, if the countdown had passed T-10s), GO LIVE is lit, and GO PUBLIC is lit (it always is). Nothing changes on its own.

After the rehearsal: return the mic fader, Auto Start/Auto Stop and any VideoHub routes to their normal settings, and delete or end the private test broadcast.

## Part C. Results

Fill this in after the rehearsal. Add a row for each run of a check.

| Date | Check no. | Pass / Fail | Notes |
|------|-----------|-------------|-------|
|      |           |             |       |
|      |           |             |       |
|      |           |             |       |
|      |           |             |       |

Go-public latency (seconds from T-10s to Public): ______ s
