# StreamIt — Live Stream Control: Workflow Design

**Date:** 2026-10-21
**Status:** Draft for review

Items marked **[call]** are decisions made while writing this document, not ones the user has stated. They are listed at the end for review.

## 1. Intent

StreamIt is a Windows desktop application, written in Rust, that runs a church's weekly YouTube live stream from a StreamDeck. Today the stream is driven by Bitfocus Companion, vMix and clicks in YouTube Studio. That setup has three problems:

- **Too many manual steps to remember.** They are spread across Companion, vMix and YouTube Studio: start the encoder, watch Studio, click Go Live, start the countdown, transition, remember to press Stop at the end.
- **The YouTube recording needs trimming every week.** It starts with minutes of holding page and countdown, and ends with the closing video and dead air.
- **Room cues drift.** The countdown light sequence runs on a Companion timer, not from the actual video time.

StreamIt replaces Companion on one of the two Companion machines, the playout PC. It replaces vMix completely. It turns each phase of the service into one button press and does the sequencing itself. The other Companion machine carries on with its other jobs. Some of those may move into StreamIt over time.

## 2. Goals, non-goals, anti-goals

### Goals (version 1)
- One-button phases: **Prepare → Countdown → (automatic) Live → End**.
- YouTube is controlled through its API:
  - The broadcast goes public at a chosen offset before the start, so the recording begins with only a short static image.
  - The app ends the broadcast itself, so the recording ends cleanly with nothing to trim.
- The app plays the countdown and roll-in videos itself through the DeckLink card. vMix is removed.
- Online viewers see a static holding image while the hall sees the countdown.
- Roll-in videos run from a cue list, one StreamDeck button per video. Each video has its own destination and audio level.
- Both ATEMs (Stream and Screens), the VideoHub route into the Stream ATEM, and the Stream ATEM's audio are switched in step with playback.
- Devices are discovered and given roles in a settings screen.

### Non-goals (version 1; may come later)
- **Countdown lights** (OSC over UDP broadcast). This is planned and the design leaves room for it (section 9), but it is not built in v1.
- **Creating YouTube broadcasts from a template.** In v1 someone still creates the broadcast in YouTube Studio beforehand.
- **Hall audio.** Video audio is de-embedded from the DeckLink SDI and sent over Dante to the audio desk, outside the app.
- **Anything the other Companion machine does.**
- **A closing video.** The end of the stream is a holding image only. Nothing happens in the hall at the end.

### Anti-goals (failure even if everything technically works)
- **The app takes the stream off air, or puts the wrong picture or sound online, through its own fault or crash.** If the app dies, the ATEMs hold their state and the Web Presenter keeps streaming. On restart the app reads the current state of the devices and resets nothing automatically (see section 8).
- **An ugly black frame goes on air** because the VideoHub is switching. VideoHub routes change only while the affected ATEM input is off program.
- **The operator still has to remember a sequence.** Any step that needs remembering belongs in the app.
- **The service is held up by the app.** The service starts on time whatever the app thinks. Warnings never stop the operator from going ahead.
- **The stream loses the mics and the operator can't get them back.** The app turns the mics down during online videos. If a video hangs, the app crashes, or someone starts talking over a video, there must always be a one-press way (**Unmute Mics** / **Recover**) to bring them back up.

## 3. Equipment and where StreamIt sits

StreamIt runs on the playout PC, the machine that runs vMix today. That PC has the DeckLink card and the StreamDeck attached.

Current addresses come from the existing Companion export (`companion/`). The app still discovers devices and lets the user assign roles; these addresses are just the starting values.

| Device | Model | Current address | Role | What StreamIt does with it |
|---|---|---|---|---|
| ATEM #1 | **ATEM Television Studio HD8 ISO** (Fairlight audio) | 192.168.4.123 | **Stream ATEM**. Its program output is the stream feed. 8 inputs. Mic/hall audio arrives on the XLR input (Fairlight input `1001`). **Input 8 is shared between Camera 8 and the DeckLink** (via the VideoHub). | Media pool stills via a media player (holding images); preview/program; transitions; Fairlight audio levels with fades |
| ATEM #2 | **ATEM Television Studio HD** | 192.168.4.98 | **Screens ATEM**. Drives the hall screens. Inputs today: 1 = Presenter, 5 = Media, 6 = Lyrics, **8 = playout (DeckLink)**. | Program switching to the DeckLink input and back |
| Blackmagic VideoHub | **40×40** | 192.168.4.100 (TCP 9990) | Routes SDI sources. The output feeding Stream ATEM input 8 normally carries **Camera 8**. It is switched to the DeckLink only while a video plays. | Switch the input-8 route to the DeckLink before a video and back to Camera 8 afterwards. It takes about 1 second and shows black in between. |
| Blackmagic Web Presenter | **Web Presenter 4K** | 192.168.4.93 (control port 9977) | Encodes the Stream ATEM output and streams it to YouTube | Start / stop streaming, read streaming status |
| Blackmagic DeckLink (local) | **DeckLink SDI 4K** | SDI playout of the countdown and roll-ins, with embedded audio (also de-embedded to Dante outside the app) | Play files |
| NDI (from this PC) | — | Countdown counter for ProPresenter on another machine | Send a rendered "time remaining" counter (not the video) |
| YouTube (Data API v3) | — | The broadcast | List upcoming broadcasts, check the stream is healthy, make the broadcast public, end it |
| StreamDeck (USB) | **Stream Deck XL** (4 rows × 8 keys = 32 keys) | Operator controls | StreamIt drives it directly. Companion must not run on this PC. |

## 4. Configuration (settings screen)

Settings are stored on disk and persist between runs.

### 4.1 Devices
- Blackmagic devices on the local network are found automatically. A device can also be added by IP address as a fallback.
- Each device is given a role: Stream ATEM, Screens ATEM, VideoHub, Web Presenter.
- Live connection status is shown for every device with a role, and for YouTube, the DeckLink and the StreamDeck.

### 4.2 Per-role settings

**Stream ATEM**
- The media player to use for holding stills.
- The media pool still for **opening holding** ("Stream starting soon...").
- The media pool still for **closing holding**. This is a different still from the opening one.
- The input used for DeckLink video (**input 8**).
- **Live inputs**: the set of inputs that count as "live action". There will be 8 cameras: **Cam 1 is the safe wide shot**, and Cams 2 and 3 are manned and usually pointing the right way. Default live inputs: **1, 2, 3**.
- **Default live input**: used when preview is not on a live input. Default: **Cam 1 (safe wide)**.
- **Mic/hall audio sources**: the XLR inputs.
- **Video duck level**: the level the mics are faded to during a video. Default: fully down (−∞). **[call]**
- **Fade time**: default 1 second. **[call]**

**Screens ATEM**
- The input that carries the DeckLink.
- The **T-0 input**: where the hall screens go when the countdown ends.

**VideoHub**
- **The output that feeds Stream ATEM input 8.**
- **The VideoHub input that carries the DeckLink.**
- **Settle time** after a route change before the input is treated as clean. Default 1.5 s. **[call]**
- The **normal route** for that output (what input 8 carries when no video is playing). This is captured from the current route at the moment a video is routed, and is configurable as a fallback.
- The Screens ATEM's DeckLink input is assumed to be fed directly or by a fixed route. StreamIt does not route it. **[call]**

**YouTube**
- Sign-in.
- The Web Presenter's YouTube stream (stream key), so the app can check that the selected broadcast is bound to it.

**Timings**
- Go-public offset. Default **T-10s**.
- Closing hold. Default **10 s**.

**DeckLink**
- Output video mode, matching the ATEMs: **1080p29.97**.

**Countdown video**
- File path. Its length defines T-0.

**NDI**
- Source name.
- Counter look: 1080p full frame, **yellow numbers on black**, adjustable.

## 5. Workflow A: Go Live

### Stage 0 — Before the service (app open, nothing happening)
1. The app connects to every configured device and shows its status in the app window and on StreamDeck status keys. This includes **whether the YouTube sign-in is valid.**
2. The app lists **upcoming** broadcasts on the channel and selects the next scheduled one. The operator can pick a different one in the app window.
3. The app checks the selected broadcast:
   - **It is bound to the Web Presenter's stream.** If it isn't, the app warns and offers a one-press "Bind" fix. It never rebinds silently. **[call]**
   - **Auto Start is OFF.** Otherwise YouTube would go public as soon as the Web Presenter starts sending. If it is on, the app warns and offers a one-press fix.
   - **Auto Stop is ON**, as a safety net (see Workflow C). If it is off, the app warns and offers a one-press fix.
   - **The monitor stream is enabled**, which YouTube needs for the private "testing" state.

### Stage 1 — Prepare (one StreamDeck button, roughly 10–20 min before)
1. **Stream ATEM:** load the opening holding still into the media player and put that media player on program. No VideoHub routing is needed.
2. **Stream ATEM audio:** the mic/hall XLR inputs go to silent, so the stream is **silent during holding**.
3. **Web Presenter:** start streaming.
4. **YouTube:** wait until the stream is active and healthy, then move the broadcast to the private **testing** state. Nothing is public yet.
5. **StreamDeck:** shows **READY** once YouTube confirms. If YouTube hasn't confirmed within a timeout, it shows a warning colour and the reason.

### Stage 2 — Countdown (one StreamDeck button)
1. **DeckLink:** start playing the countdown video. T-0 is the moment the video ends.
2. **Screens ATEM:** switch program to the DeckLink input. The hall sees the countdown.
3. **NDI:** send a time-remaining counter for ProPresenter, driven from the real playback position.
4. **Stream ATEM:** no change. Online viewers still see the static opening holding image, in silence.
5. **At the go-public offset (T-10s):** the app moves the YouTube broadcast from testing to **live**. The public and the recording start here, on the static image. If this fails, the app retries and shows a red key. A manual **Go Public** key is always available.
6. If Stage 1 is not READY when Countdown is pressed, the app **warns but continues**.
7. A **Stop Countdown** key aborts the countdown:
   - Playback stops.
   - The Screens ATEM goes to its T-0 input.
   - The YouTube state is left as it is. **[call]**

### Stage 3 — T-0 (automatic, when the countdown video ends)
1. **Stream ATEM:**
   - If preview is not one of the configured **live inputs**, set preview to the **default live input**.
   - Run an Auto transition (using the ATEM's current transition settings) so preview goes to program.
   - **Fade the mic/hall XLR inputs up** to their live level over the fade time.
2. **Screens ATEM:** switch program to the configured **T-0 input**.
3. **NDI:** the counter shows 0:00, then clears.
4. **StreamDeck:** shows **LIVE**.

## 6. Workflow B: Roll-in videos

### 6.1 The cue list
- In the app window, the operator picks video files on the playout PC and adds them to the cue list for the service.
- **Each video gets one StreamDeck button, in the order the videos were added.** The button shows the video's name and its state: cued, playing or played.
- Each video has these settings:
  - **Destination**: `Hall + Online`, `Hall only`, `Online only` or `Audio only`.
  - **Level**: a gain trim in dB, applied during playback.
  - **Return input** (optional). This is the Stream ATEM input to go back to after the video. If it is empty, the app returns to **whatever is on Preview**, because other people operate the Stream ATEM during the service.

### 6.2 Playing a video (one button press)

The sequence is ordered so the VideoHub's black gap never goes on air.

1. **Load the file into the DeckLink, paused on its first frame.** Video and audio are held ready.
2. **If the destination needs the Stream ATEM** (Hall + Online, Online only, Audio only):
   1. **Safety check:** if Stream ATEM **program** is currently input 8, rerouting it would put black on air. In that case the app does not route. It shows a warning, and the operator can retry once input 8 is off program. **[call]**
   2. Record the current route for the input-8 VideoHub output. That becomes the "normal route" to restore.
   3. Switch the VideoHub route so input 8 carries the DeckLink, then wait the settle time.
3. **Switch the picture and audio** according to the table below:
   - **Mic/hall XLR fade** means a smooth fade down to the duck level over the fade time.
   - **Input 8 audio** is the Fairlight input-8 source, set to on/unity plus the per-video trim.

| Destination | Stream ATEM picture | Input 8 audio | Mic/hall XLR | Screens ATEM picture |
|---|---|---|---|---|
| Hall + Online | Auto transition to input 8 | on | fade down | switch to DeckLink input |
| Hall only | no change | — | no change **[call]** | switch to DeckLink input |
| Online only | Auto transition to input 8 | on | fade down | no change |
| Audio only | no change | on | fade down | no change |

4. **Play.** Playback starts as the transition starts. **[call]**
5. **When the video ends**, or when **Stop** is pressed:
   1. **Stream ATEM:**
      - Set preview to the video's return input, if it has one.
      - Auto transition from input 8 to **Preview**.
      - Fade the mic/hall XLR inputs back up.
      - Fade input 8 audio out.
   2. **Screens ATEM:** switch back to **whatever is on its Preview**, the same rule as the Stream ATEM. **[call]**
   3. **Only after input 8 is off program:** restore the VideoHub route for input 8 to its normal source.
6. **Unmute Mics key.** It is shown while a video has the mics faded down. Pressing it fades the mics back up straight away, and the video keeps playing.
7. **One video at a time.** Pressing another video while one is playing does nothing. The key flashes to say it was ignored. **[call]**

### 6.3 Stream ATEM audio policy (summary)
- **Holding (opening and closing):** the mic/hall XLR inputs are silent, so the stream is silent.
- **Live:** the mic/hall XLR inputs are at their live level.
- **During a video going online:** the mic/hall inputs are faded down (not hard-muted) and input 8 audio is on. Both fade back after the video. **Unmute Mics** overrides the fade.
- **"Live level"** is the level each XLR input was at before the app first faded it. The app records it at Prepare and before each fade, so whatever the sound team has set is respected. **[call]**

## 7. Workflow C: Stream End

1. **End Service** (one StreamDeck button):
   - Stream ATEM: load the closing holding still into the media player and Auto transition program to it.
   - The mic/hall XLR inputs fade to silent.
2. **Closing hold:** the app waits for the configured duration. The key shows a countdown. Pressing it again during the hold ends the broadcast straight away. **[call]**
3. **YouTube:** move the broadcast to **complete**. The recording ends on the holding still.
4. **Web Presenter:** stop streaming.
5. **Safety net:** Auto Stop is on for the broadcast. If the app failed before step 3, YouTube ends the broadcast once the Web Presenter stops sending.
6. **StreamDeck:** shows **ENDED**.

## 8. Failures and recovery

- **A device disconnects:**
  - The app shows a red status key and reconnects automatically.
  - Any action that needs that device warns instead of silently failing. The rest of the workflow carries on.
- **The app crashes or restarts mid-service:**
  - The devices keep their current state.
  - On start, the app reads the ATEMs, the VideoHub, the Web Presenter and YouTube, and works out which stage the service is in. It never changes devices automatically on start-up.
  - **Crash during a video.** If the app finds an ATEM on the DeckLink input with nothing playing, or the input-8 route left on the DeckLink, or the mics faded down, it lights a **Recover** key. One press:
    - cuts back to Preview;
    - fades the mics up;
    - restores the VideoHub route.

    Nothing happens automatically. **[call]**
  - **Crash during the countdown.** The automatic go-public and T-0 steps do not resume. Their keys (**Go Public**, **Go Live**) light up to be pressed by hand. **[call]**
- **The YouTube API fails:** the app retries with backoff and shows a clear status. The manual **Go Public** and **End Broadcast** keys are always available.
- **YouTube sign-in:** use a Google Cloud project with the OAuth consent screen published, not in "testing" mode. Otherwise the sign-in expires every 7 days. The pre-service status check (Stage 0) flags an expired or invalid sign-in. **[call]**
- **Manual override:** every automatic step has a manual equivalent on the StreamDeck or in the app window.
- **Event log:** a timestamped log of every action and every device response is written to disk.

## 9. Later: countdown lights

The light system takes OSC commands over UDP broadcast. A later version will send the colour changes, driven from the countdown's **actual playback position**. So the playback engine should expose playback position and events that a later light module can subscribe to.

**What the Companion export shows about today's setup** (to confirm when this work starts):
- **Target:** UDP broadcast to `192.168.4.255:8765`.
- **Colour on:** `/<stage>/led/on` with arguments `255 R G B`. The stage is a variable, currently `linux`.
- **Off:** `/<stage>/led/off 255`.
- **Today's sequence** (button "30s Cntdwn"):

  | When | Colour | Arguments |
  |---|---|---|
  | T-30 | blue | `255 0 0 255` |
  | T-15 | green | `255 0 255 0` |
  | T-5 | yellow | `255 255 255 0` |
  | T-0 ("GO LIVE") | red | `255 255 0 0` |

- **Signals at the end of the service, handled in the same piece of work.** Today's "Ending" button also sends:
  - `{"cmd":"off","target":"Audio"}` to a flash/notification system (UDP broadcast `192.168.4.255:7000`);
  - a red signal to the youth room (`172.16.1.255:7000`).

## 10. Left to the builder

- Libraries and protocols for each device. Options include:
  - the Blackmagic SDKs (COM) or published network protocols for ATEM, VideoHub (TCP 9990) and the Web Presenter 4K's control protocol;
  - FFmpeg or GStreamer for video decode;
  - the DeckLink SDK and the NDI SDK;
  - StreamDeck HID.
- The UI toolkit for the app window.
- The StreamDeck key layout, including paging if the cue list has more videos than there are keys.
- How configuration is stored, and how the YouTube OAuth token is stored.
- Polling and timeout values, retry policy and log format.

## 11. Calls made in this document (for review)

1. Default mic level during a video is fully down (−∞), with a 1 s fade. Both are configurable.
2. VideoHub settle time is 1.5 s, configurable. Playback starts as the ATEM transition starts.
3. If input 8 is on program when a video is pressed, the app refuses to reroute and warns.
4. StreamIt routes the VideoHub only for Stream ATEM input 8. The Screens ATEM's DeckLink feed is assumed to be fixed.
5. **Hall only** videos leave the stream's mic audio alone.
6. The Screens ATEM also returns to its Preview after a video.
7. "Live level" for the mics is captured from the desk/ATEM before the app fades them, not set in StreamIt.
8. Pressing a second video while one plays is ignored.
9. **Stop Countdown** sends the hall to its T-0 input and leaves YouTube alone.
10. Pressing the key again during the closing hold ends it early.
11. After a crash, nothing recovers automatically. **Recover**, **Go Public** and **Go Live** keys light up for the operator.
12. Broadcast binding, Auto Start and Auto Stop are fixed with one press after a warning, never silently.
13. The NDI counter is 1080p full frame.
14. The YouTube Cloud project must be published, so the sign-in does not expire every 7 days.

## 12. Still unknown (not blocking planning)

- The VideoHub input numbers for Camera 8 and the DeckLink, and the VideoHub output feeding Stream ATEM input 8. These are set in configuration.
- Which Screens ATEM input is the T-0 input. This is set in configuration (the current Pre-Stream button uses Lyrics, input 6).
