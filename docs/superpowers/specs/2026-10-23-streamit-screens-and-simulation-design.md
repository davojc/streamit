# StreamIt v1.1 — Operator Screens and Simulation Mode: Design

**Date:** 2026-10-23
**Status:** Draft for review
**Builds on:** `2026-10-21-streamit-workflows-design.md` (the v1 workflows; still binding where not changed here)
**Screen mockups (binding for layout and content):** https://claude.ai/artifact/PEFzDp4XbnGF48ZGoANrFj

Items marked **[call]** are decisions made while writing this, not stated by the user.

## 1. Intent

v1 works, but its window was left "to the builder" and does not match what the operator needs. v1.1 does five things:

- **Rebuilds the window** to the approved mockups.
- **Fixes the confusing Go Live / Go Public split.**
- **Makes the NDI counter follow every video.**
- **Closes the three issues left open after the v1 final review.**
- **Adds a simulation mode**, so the operator can practise every workflow with no equipment and no YouTube.

## 2. Goals, non-goals, anti-goals

**Goals**
- The window matches the seven mockup screens:
  - before the service;
  - countdown;
  - live;
  - live with a video playing;
  - videos (prep);
  - settings;
  - simulation.
- One operator action, **Start service now**, replaces separate Go Live and Go Public in normal use.
- The NDI counter shows the time remaining of **any** playing video.
- A simulation mode, switched on and off by a button in the app, that runs the real engine and sequencers against pretend equipment and a pretend YouTube, with fault injection.

**Non-goals**
- Lights and OSC.
- Creating a broadcast from a template.
- New Stream Deck layouts. The deck keeps v1's layout, apart from the label changes in section 4.
- Anything about the NDI receiver. StreamIt only sends the feed.

**Anti-goals** (in addition to v1's)
- **Simulation must never touch real equipment or YouTube**, even if the network is up and the settings point at real addresses.
- **Simulation must never be mistaken for live.** It is unmistakable on screen and on the Stream Deck.
- **Simulation must not change real service state.** It never overwrites the real saved mic levels, and nothing it writes leaks into a real run.

## 3. The screens

Layout, wording, colours and structure follow the mockups.

### 3.1 Every screen
- **Header:**
  - the StreamIt name;
  - a stage badge: IDLE grey, READY green, COUNTDOWN amber, LIVE red;
  - the on-air timer when live;
  - the broadcast title;
  - an equipment summary ("All equipment OK" or the problem);
  - tabs: **Service, Videos, Settings**, plus **Simulation** in simulation mode only.
- **Service step strip:** Prepare → Countdown → Live → End service. Done steps show green ticks and the current step is highlighted.

### 3.2 Service tab
The tab shows one of the four mockups, chosen by stage.

- **Before the service:**
  - **Next step** panel with a big **Prepare** button, and previews of what online viewers and the hall will see.
  - **Pre-flight checks:** the YouTube checks, with **Fix** buttons on amber items.
  - **Equipment:** each device with its own status.
  - The videos lined up for the service.
- **Countdown:**
  - A large "Countdown video ends in m:ss": the countdown **video's** remaining time, plus its file name.
  - What happens at T-10 and T-0.
  - The NDI counter's current value.
  - Previews of what online viewers and the hall see.
  - Buttons: **Start service now** and **Stop countdown**.
  - A **Retry going public** box, shown only after going public has failed.
  - An activity list.
- **Live:**
  - What's on air for the stream (program, preview, mic level) and for the hall.
  - YouTube status.
  - **Next step: End service**.
  - The activity list.
  - The video list, with a **Play** button on each video.
- **Live, video playing:**
  - Now playing: the video's name, time remaining, a progress bar and the NDI counter value.
  - **Stop video** and **Unmute mics**.
  - What's on air, with "returns to …" for each switcher.
  - The other videos greyed out.

### 3.3 Videos tab (new)
- **Countdown video:** the file name, its length and **Change…**.
- **Videos for this service:**
  - Rows in playing order, at most 14. Each row shows:
    - the number and its Stream Deck key (keys 17–23, then 25–31);
    - a thumbnail placeholder, the name, the file name and the length;
    - where it plays and its level;
    - move up, move down and remove buttons.
  - **Add videos…** at the top of the list.
- **Selected video panel:**
  - the name shown on the Stream Deck key;
  - a first-frame preview **[call: a placeholder box in v1.1; real thumbnails later]**;
  - four "where it plays" buttons, each with a plain explanation;
  - a level slider from −20 to +6 dB;
  - "Afterwards, the stream goes back to": whatever is on preview (the usual choice), or a named input.
- **Saving:** edits save immediately (via `SetCues`). Edits are refused, with a message, while a video is playing.

### 3.4 Settings tab
- **Sections:**
  - Equipment: discovered devices with a role dropdown, Search again, Add by IP.
  - Inputs and routing, with plain-language labels as in the mockup.
  - Timings and audio.
  - YouTube.
  - Playout and NDI counter.
- **Saving:** Save and Discard buttons.
- **Restart note:** "Changes to equipment take effect when StreamIt restarts" (v1's R19 still holds).
- **Simulation card:** Equipment holds the Simulation switch (section 7.1).

### 3.5 Look
- **Colours:**
  - dark control-room ground `#0E1116`, panels `#161B22`;
  - red `#E5484D` = on air;
  - green `#3DD68C` = OK / ready;
  - amber `#F5A524` = warning / countdown / simulation;
  - blue `#4C9AFF` = video and selection.
- **Type:** Barlow Semi Condensed for labels and headings, Atkinson Hyperlegible for body text, JetBrains Mono for times. Fonts are bundled, not fetched.
- **Toolkit:** stays egui. Rebuilding the look in egui is the builder's job; the mockups are the target. **[call]**

## 4. Start service now (replaces Go Live / Go Public in normal use)

- **What the button does:** **Start service now** (window), the **GO LIVE** key (deck, label unchanged), and the countdown reaching T-0 all run one sequence:
  1. if the broadcast is not yet public, make it public;
  2. **and in parallel**, the T-0 cut.
  - It does not wait for YouTube before cutting. A go-public failure becomes a warning plus the **Retry going public** action.
- **When it's accepted:** from Prepared and Countdown, the same rules as v1's R22/R25. During a countdown it pre-empts as before (R13).
- **Go public on its own:**
  - shown in the window only as **Retry going public**, after a failure or when the broadcast is live on our stream but not public;
  - the deck's GO PUBLIC key stays (always pressable, as in v1).

## 5. NDI counter for every video

- While **any** video plays (the countdown or a roll-in), the counter shows `M:SS` remaining, updated every 100 ms, as the countdown does in v1.
- It clears when the video ends or is stopped.
- One source name: "StreamIt Countdown".

## 6. The three open issues from the v1 final review

1. **Startup check:** only the YouTube part of the startup and refresh check is time-limited. The device read always runs and its result is always applied.
2. **Default broadcast:** when nothing is selected, prefer a testing/live broadcast bound to our stream, then the first *upcoming* broadcast. Never default to a stale or foreign active one.
3. **UNMUTE key:** UNMUTE is only lit while a video plays. A failed mic fade outside a video offers **Recover**, as now.

Also: the spec, README and rehearsal checklist stop mentioning ProPresenter.

## 7. Simulation mode

### 7.1 Switching in and out
- **Where:** a dashed-amber **Simulation** card in **Settings → Equipment**, with **Switch to simulation** (and **Leave simulation** while simulating).
- **From the banner:** the simulation banner links to that card ("Leave simulation in Settings › Equipment").
- **When you can switch:** in either direction, only while nothing is happening: stage Idle or Ended, and no sequence running. Otherwise the button is disabled, with the reason given ("Finish or end the service first").
- **What switching does:** stops the engine, rebuilds the device layer (pretend ↔ real), and starts the engine again. Switching back to real does the normal read-only startup (v1: never changes a device at startup).
- **Restarts:** the app always starts in **real** mode, whatever mode it was in when it closed. **[call]**
- In simulation the app:
  - constructs **no** real device clients: no ATEM, VideoHub, Web Presenter, DeckLink, NDI or YouTube connections, and no mDNS discovery;
  - uses the real settings file (input numbers, timings, cues), so practice matches the real setup;
  - keeps its saved mic levels in a separate `sim-levels.toml`, and writes logs to a separate `sim-` log file.
- The Stream Deck still works, so the operator practises on the real keys. Its phase key shows **SIM** under the stage. **[call]**

### 7.2 Pretend equipment
Pretend equipment behaves like the real kit, at the selected speed. The real engine, sequencers and Stream Deck code run unchanged; only the device layer is swapped.

| Pretend device | Behaviour |
|---|---|
| Stream ATEM | A mix takes 1 s. During the mix, `program()` still reports the old source (as on real hardware). Mic levels, input-8 audio and the media-player still all behave. |
| Screens ATEM | Same mix behaviour. |
| VideoHub | A route takes effect after 1 s. |
| Web Presenter | Connecting → Streaming after 3 s. |
| YouTube | Pretend broadcasts (e.g. "Test service", upcoming and bound to our stream). Health Good 5 s after streaming starts. Testing/live/complete transitions take 2–3 s. Fixes apply. |
| DeckLink player | "Plays" for the file's real length, read from the file (mp4/mov) **[call]**, with a 30 s fallback if the length can't be read. Position advances in real time × speed. |
| NDI counter | Its current value is shown on the Simulation tab. Nothing is sent. |

### 7.3 Simulation tab
Laid out as in the mockup.

- **Stream ATEM controls.** The operator plays the vision mixer:
  - program and preview source buttons;
  - Cut and Auto;
  - mic level, input-8 audio and what input 8 carries.
  - This is how you test "return to whatever is on preview".
- **Other equipment states:** Screens program, Web Presenter, DeckLink and the NDI counter value.
- **Pretend YouTube state:** the broadcast's state and stream health, and how long the last go-public took.
- **"Make something go wrong" buttons**, each breaking one thing:
  - unplug the VideoHub, Stream ATEM or Web Presenter;
  - YouTube refuses to go public;
  - YouTube very slow (calls take 20 s);
  - the next video won't load;
  - the mic fade fails;
  - **crash and restart the app** (restarts the engine against the pretend equipment as it currently stands, to practise Recover).
  - Pressing a button again restores the thing it broke.
- **Speed:** 1×, 4× or 10×. It scales videos, waits, retries and holds. The real service is always 1×.
- **Reset simulation:** puts every pretend device back to before the service.

### 7.4 Always visible
- A striped amber banner on every screen: "SIMULATION — nothing here is live. No equipment or YouTube is touched."
- The window title is suffixed "— SIMULATION".

## 8. Left to the builder
- How to restyle egui (custom visuals, bundled fonts, layout widgets) to reach the mockups.
- Reading the length of mp4/mov files (a pure-Rust crate, so it works without GStreamer).
- Where the pretend devices live: a new `streamit-sim` crate is suggested; reusing and extending the core fakes is fine.
- The internal API for fault injection and speed.

## 9. Calls made in this document (for review)
1. Simulation is switched in Settings → Equipment, only when nothing is happening, and the app always starts in real mode. (The user decided the in-app switch and its place; the "only when idle" limit and the start-in-real rule are calls.)
2. The Stream Deck works in simulation, and its phase key shows SIM.
3. Simulated videos play for the file's real length, or 30 s if it can't be read.
4. The first-frame preview on the Videos tab is a placeholder in v1.1.
5. The window stays egui, restyled to the mockups.
