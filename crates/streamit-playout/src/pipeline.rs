//! Pure builders for the GStreamer pipeline descriptions, plus gain and counter text.

use std::path::Path;
use std::time::Duration;

pub use streamit_core::audio::FLOOR_DB;

/// Where the player's video and audio go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// The DeckLink SDI output (video and embedded audio).
    DeckLink,
    /// `fakesink sync=true` for both branches, for tests without hardware.
    Fake,
}

/// The audio branch of the player pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// The file's own audio track.
    File,
    /// The file has no audio: silence for `duration`, so the DeckLink audio stays clocked
    /// and the pipeline still reaches end-of-stream when the video ends.
    Silence { duration: Duration },
}

/// Caps for the audio the DeckLink gets: 48 kHz stereo.
pub const AUDIO_CAPS: &str = "audio/x-raw,rate=48000,channels=2";

/// The default `playout.video_mode`.
pub const DEFAULT_MODE: &str = "1080p2997";

/// Silence is generated in 10 ms buffers (480 samples at 48 kHz).
const SILENCE_SAMPLES_PER_BUFFER: u64 = 480;

/// A DeckLink output mode: the `decklinkvideosink` `mode` nick and its frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub nick: &'static str,
    pub fps_num: u32,
    pub fps_den: u32,
}

/// The 1080 progressive `GstDecklinkModes` nicks, from `gst_decklink_mode_get_type` in
/// gst-plugins-bad `sys/decklink/gstdecklink.cpp` (e.g.
/// `{GST_DECKLINK_MODE_1080p2997, "HD1080 29.97p", "1080p2997"}`). Interlaced and non-1080
/// modes are refused, because the player scales to fixed 1920x1080 progressive caps.
const MODES: &[VideoMode] = &[
    VideoMode {
        nick: "1080p2398",
        fps_num: 24000,
        fps_den: 1001,
    },
    VideoMode {
        nick: "1080p24",
        fps_num: 24,
        fps_den: 1,
    },
    VideoMode {
        nick: "1080p25",
        fps_num: 25,
        fps_den: 1,
    },
    VideoMode {
        nick: "1080p2997",
        fps_num: 30000,
        fps_den: 1001,
    },
    VideoMode {
        nick: "1080p30",
        fps_num: 30,
        fps_den: 1,
    },
    VideoMode {
        nick: "1080p50",
        fps_num: 50,
        fps_den: 1,
    },
    VideoMode {
        nick: "1080p5994",
        fps_num: 60000,
        fps_den: 1001,
    },
    VideoMode {
        nick: "1080p60",
        fps_num: 60,
        fps_den: 1,
    },
];

impl VideoMode {
    /// Maps `playout.video_mode` from the config (a `decklinkvideosink` mode nick).
    pub fn from_nick(nick: &str) -> Option<VideoMode> {
        MODES.iter().copied().find(|m| m.nick == nick)
    }

    /// Raw video caps the DeckLink is fed in this mode.
    pub fn video_caps(&self) -> String {
        format!(
            "video/x-raw,width=1920,height=1080,framerate={}/{}",
            self.fps_num, self.fps_den
        )
    }
}

impl Default for VideoMode {
    fn default() -> Self {
        VideoMode::from_nick(DEFAULT_MODE).expect("default mode is in the table")
    }
}

/// dB to a linear `volume` factor. At or below the Fairlight floor (-100 dB) it is 0.0 (R4).
pub fn gain_db_to_linear(db: f32) -> f64 {
    if db <= FLOOR_DB {
        0.0
    } else {
        10f64.powf(f64::from(db) / 20.0)
    }
}

/// Counter text, `M:SS`. Part seconds round up, so it reads `0:00` only at zero.
pub fn counter_text(remaining: Duration) -> String {
    let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// An opaque ARGB colour as GStreamer's `color` properties take it.
pub fn argb([r, g, b]: [u8; 3]) -> u32 {
    0xFF00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// A `file://` URI for an absolute path, percent-encoded so it needs no escaping inside a
/// pipeline description. Handles drive paths, `\\?\` verbatim paths and UNC shares.
pub fn file_uri(path: &Path) -> String {
    let mut s = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        s = format!("//{rest}");
    } else if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    let (prefix, rest) = if let Some(rest) = s.strip_prefix("//") {
        ("file://", rest)
    } else if let Some(rest) = s.strip_prefix('/') {
        ("file:///", rest)
    } else {
        ("file:///", s.as_str())
    };
    let mut out = String::from(prefix);
    for b in rest.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Number of 10 ms silence buffers covering `duration` (at least one).
pub fn silence_buffers(duration: Duration) -> u64 {
    let samples = (duration.as_nanos() * 48_000).div_ceil(1_000_000_000);
    (samples as u64).div_ceil(SILENCE_SAMPLES_PER_BUFFER).max(1)
}

/// Quotes a string for a pipeline description.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn video_sink(sink: SinkKind, device: u32, mode: VideoMode) -> String {
    match sink {
        SinkKind::DeckLink => {
            format!(
                "decklinkvideosink device-number={device} mode={}",
                mode.nick
            )
        }
        SinkKind::Fake => "fakesink sync=true".into(),
    }
}

fn audio_sink(sink: SinkKind, device: u32) -> String {
    match sink {
        SinkKind::DeckLink => format!("decklinkaudiosink device-number={device}"),
        SinkKind::Fake => "fakesink sync=true".into(),
    }
}

/// The player pipeline for a file with audio, in the default mode (1080p29.97).
pub fn player_pipeline(path: &Path, gain_db: f32, sink: SinkKind, device: u32) -> String {
    player_pipeline_full(
        &file_uri(path),
        gain_db,
        sink,
        device,
        VideoMode::default(),
        AudioSource::File,
    )
}

/// The player pipeline. Any input is converted, scaled and rate-converted to the DeckLink's
/// mode; audio is converted to 48 kHz stereo with the cue's gain applied.
///
/// The decoder's pads link by caps: `dec. ! videoconvert` only accepts a video pad and
/// `dec. ! audioconvert` only an audio pad, so each branch gets the right stream.
///
/// `deinterlace` (which passes progressive video through) makes interlaced files (1080i,
/// 576i) fit the progressive caps. A `videoconvert` sits before it so any decoded format
/// (e.g. 10-bit) reaches it in one it accepts, and the delayed link stays video-only.
pub fn player_pipeline_full(
    uri: &str,
    gain_db: f32,
    sink: SinkKind,
    device: u32,
    mode: VideoMode,
    audio: AudioSource,
) -> String {
    let video = format!(
        "dec. ! videoconvert ! deinterlace ! videoconvert ! queue ! videoscale ! videorate ! \
         {} ! {}",
        mode.video_caps(),
        video_sink(sink, device, mode)
    );
    let audio_in = match audio {
        AudioSource::File => "dec. ! audioconvert".to_string(),
        AudioSource::Silence { duration } => format!(
            "audiotestsrc wave=silence samplesperbuffer={SILENCE_SAMPLES_PER_BUFFER} \
             num-buffers={} ! {AUDIO_CAPS} ! audioconvert",
            silence_buffers(duration)
        ),
    };
    let audio = format!(
        "{audio_in} ! queue ! audioresample ! volume volume={:.6} ! {AUDIO_CAPS} ! {}",
        gain_db_to_linear(gain_db),
        audio_sink(sink, device)
    );
    format!("uridecodebin uri={} name=dec {video} {audio}", quoted(uri))
}

/// What the DeckLink outputs after `stop`: black with silent audio.
pub fn black_pipeline(device: u32, mode: VideoMode) -> String {
    format!(
        "videotestsrc pattern=black is-live=true ! {} ! {} \
         audiotestsrc wave=silence is-live=true ! {AUDIO_CAPS} ! {}",
        mode.video_caps(),
        video_sink(SinkKind::DeckLink, device, mode),
        audio_sink(SinkKind::DeckLink, device)
    )
}

/// The NDI counter: a live full-frame background with a centred text overlay named
/// `counter`, whose `text` property is updated while it runs.
pub fn counter_pipeline(source_name: &str, fg: [u8; 3], bg: [u8; 3]) -> String {
    format!(
        "videotestsrc pattern=solid-color foreground-color=0x{:08X} is-live=true ! {} ! \
         textoverlay name=counter color=0x{:08X} halignment=center valignment=center \
         auto-resize=false font-desc=\"Sans Bold 300px\" shaded-background=false \
         draw-shadow=false draw-outline=false ! \
         videoconvert ! ndisink ndi-name={}",
        argb(bg),
        VideoMode::default().video_caps(),
        argb(fg),
        quoted(source_name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const VCAPS: &str = "video/x-raw,width=1920,height=1080,framerate=30000/1001";
    const ACAPS: &str = "audio/x-raw,rate=48000,channels=2";

    fn mode() -> VideoMode {
        VideoMode::from_nick("1080p2997").unwrap()
    }

    #[test]
    fn gain_db_to_linear_matches_r4() {
        assert!((gain_db_to_linear(-6.0) - 0.501).abs() < 0.001);
        assert_eq!(gain_db_to_linear(0.0), 1.0);
        assert!((gain_db_to_linear(6.0) - 1.995).abs() < 0.001);
        assert_eq!(gain_db_to_linear(FLOOR_DB), 0.0);
        assert_eq!(gain_db_to_linear(-150.0), 0.0);
        assert!(gain_db_to_linear(-99.0) > 0.0);
    }

    #[test]
    fn counter_text_is_m_ss() {
        assert_eq!(counter_text(Duration::from_secs(125)), "2:05");
        assert_eq!(counter_text(Duration::from_secs(0)), "0:00");
        assert_eq!(counter_text(Duration::from_secs(9)), "0:09");
        assert_eq!(counter_text(Duration::from_secs(60)), "1:00");
        assert_eq!(counter_text(Duration::from_secs(3725)), "62:05");
        // Part seconds round up, so the counter reads 0:00 only at zero.
        assert_eq!(counter_text(Duration::from_millis(124_001)), "2:05");
        assert_eq!(counter_text(Duration::from_millis(400)), "0:01");
    }

    #[test]
    fn argb_is_opaque_rgb() {
        assert_eq!(argb([255, 214, 0]), 0xFFFF_D600);
        assert_eq!(argb([0, 0, 0]), 0xFF00_0000);
    }

    #[test]
    fn video_mode_maps_config_to_decklink_nick() {
        // Nick from GEnumValue {GST_DECKLINK_MODE_1080p2997, "HD1080 29.97p", "1080p2997"}
        // in gst-plugins-bad sys/decklink/gstdecklink.cpp.
        let m = VideoMode::from_nick("1080p2997").unwrap();
        assert_eq!(m.nick, "1080p2997");
        assert_eq!(m.video_caps(), VCAPS);
        assert_eq!(
            VideoMode::from_nick("1080p25").unwrap().video_caps(),
            "video/x-raw,width=1920,height=1080,framerate=25/1"
        );
        assert_eq!(VideoMode::from_nick("1080i5994"), None);
        assert_eq!(VideoMode::from_nick("bogus"), None);
    }

    #[test]
    fn file_uri_percent_encodes_windows_paths() {
        assert_eq!(
            file_uri(Path::new(r"C:\Videos\Notices 1.mp4")),
            "file:///C:/Videos/Notices%201.mp4"
        );
        assert_eq!(file_uri(Path::new(r"\\?\C:\a\b.mp4")), "file:///C:/a/b.mp4");
        assert_eq!(
            file_uri(Path::new(r"\\server\share\x.mp4")),
            "file://server/share/x.mp4"
        );
        assert_eq!(
            file_uri(Path::new(r#"C:\a"b'#%.mp4"#)),
            "file:///C:/a%22b%27%23%25.mp4"
        );
        assert_eq!(file_uri(Path::new("/tmp/clip.mp4")), "file:///tmp/clip.mp4");
    }

    #[test]
    fn silence_runs_for_the_clip_duration_in_10ms_buffers() {
        assert_eq!(silence_buffers(Duration::from_secs(2)), 200);
        assert_eq!(silence_buffers(Duration::from_millis(2001)), 201);
        assert_eq!(silence_buffers(Duration::ZERO), 1);
    }

    #[test]
    fn pipeline_converts_any_input() {
        let p = player_pipeline(
            Path::new(r"C:\Videos\clip.mp4"),
            -6.0,
            SinkKind::DeckLink,
            0,
        );
        assert!(
            p.contains(r#"uridecodebin uri="file:///C:/Videos/clip.mp4" name=dec"#),
            "{p}"
        );
        assert!(
            p.contains(&format!(
                "dec. ! videoconvert ! deinterlace ! videoconvert ! queue ! videoscale ! \
                 videorate ! {VCAPS} ! decklinkvideosink device-number=0 mode=1080p2997"
            )),
            "interlaced input is deinterlaced before the progressive caps: {p}"
        );
        assert!(
            p.find("deinterlace").unwrap() < p.find(VCAPS).unwrap(),
            "deinterlace comes before the progressive caps: {p}"
        );
        assert!(p.contains("dec. ! videoconvert ! deinterlace"), "{p}");
        assert!(
            p.contains(&format!(
                "dec. ! audioconvert ! queue ! audioresample ! volume volume=0.501187 ! {ACAPS} ! \
                 decklinkaudiosink device-number=0"
            )),
            "{p}"
        );
        assert!(!p.contains("audiotestsrc"), "{p}");
        assert!(!p.contains("fakesink"), "{p}");
    }

    #[test]
    fn device_number_goes_to_both_decklink_sinks() {
        let p = player_pipeline(Path::new(r"C:\clip.mp4"), 0.0, SinkKind::DeckLink, 2);
        assert!(
            p.contains("decklinkvideosink device-number=2 mode=1080p2997"),
            "{p}"
        );
        assert!(p.contains("decklinkaudiosink device-number=2"), "{p}");
        assert!(p.contains("volume volume=1.000000"), "{p}");
    }

    #[test]
    fn fake_sink_replaces_both_decklink_sinks() {
        let p = player_pipeline(Path::new(r"C:\clip.mp4"), 0.0, SinkKind::Fake, 0);
        assert!(!p.contains("decklink"), "{p}");
        assert_eq!(p.matches("fakesink sync=true").count(), 2, "{p}");
        assert!(p.contains(&format!("{VCAPS} ! fakesink sync=true")), "{p}");
        assert!(p.contains(&format!("{ACAPS} ! fakesink sync=true")), "{p}");
    }

    #[test]
    fn file_without_audio_gets_bounded_silence() {
        let p = player_pipeline_full(
            "file:///C:/clip.mp4",
            -3.0,
            SinkKind::DeckLink,
            0,
            mode(),
            AudioSource::Silence {
                duration: Duration::from_secs(2),
            },
        );
        assert_eq!(
            p.matches("dec.").count(),
            1,
            "only the video branch uses the decoder: {p}"
        );
        assert!(
            p.contains(&format!(
                "audiotestsrc wave=silence samplesperbuffer=480 num-buffers=200 ! {ACAPS} ! \
                 audioconvert ! queue ! audioresample ! volume volume=0.707946 ! {ACAPS} ! \
                 decklinkaudiosink device-number=0"
            )),
            "{p}"
        );
        assert!(p.contains(&format!("{VCAPS} ! decklinkvideosink")), "{p}");
    }

    #[test]
    fn floor_gain_mutes() {
        let p = player_pipeline(&PathBuf::from(r"C:\clip.mp4"), FLOOR_DB, SinkKind::Fake, 0);
        assert!(p.contains("volume volume=0.000000"), "{p}");
    }

    #[test]
    fn black_pipeline_holds_black_and_silence_on_the_decklink() {
        let p = black_pipeline(1, mode());
        assert_eq!(
            p,
            format!(
                "videotestsrc pattern=black is-live=true ! {VCAPS} ! \
                 decklinkvideosink device-number=1 mode=1080p2997 \
                 audiotestsrc wave=silence is-live=true ! {ACAPS} ! \
                 decklinkaudiosink device-number=1"
            )
        );
    }

    #[test]
    fn counter_pipeline_is_yellow_on_black_ndi() {
        let p = counter_pipeline("StreamIt Countdown", [255, 214, 0], [0, 0, 0]);
        assert_eq!(
            p,
            format!(
                "videotestsrc pattern=solid-color foreground-color=0xFF000000 is-live=true ! \
                 {VCAPS} ! \
                 textoverlay name=counter color=0xFFFFD600 halignment=center valignment=center \
                 auto-resize=false font-desc=\"Sans Bold 300px\" shaded-background=false \
                 draw-shadow=false draw-outline=false ! \
                 videoconvert ! ndisink ndi-name=\"StreamIt Countdown\""
            )
        );
    }

    #[test]
    fn counter_pipeline_escapes_the_ndi_name() {
        let p = counter_pipeline(r#"Big "Hall" \ 1"#, [1, 2, 3], [4, 5, 6]);
        assert!(p.contains(r#"ndi-name="Big \"Hall\" \\ 1""#), "{p}");
        assert!(p.contains("color=0xFF010203"), "{p}");
        assert!(p.contains("foreground-color=0xFF040506"), "{p}");
    }
}
