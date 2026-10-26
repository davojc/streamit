//! ATEM command framing, the commands StreamIt sends, and the ones it reads.
//!
//! Byte layouts are ported from atem-connection (MIT, github.com/nrkno/sofie-atem-connection,
//! `main` at 9c77b69919d10108c92231903023acf4cf1cf7db). The golden byte arrays in the tests come
//! from its `src/commands/__tests__/libatem-data.json`, the fixtures its own serializer and
//! deserializer tests run against.

/// The command header is u16 BE total length, two unused bytes, then the 4-character name.
pub const COMMAND_HEADER_LEN: usize = 8;

/// One command inside a packet payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawCommand<'a> {
    pub name: [u8; 4],
    pub body: &'a [u8],
}

/// Splits a packet payload into commands.
/// Upstream: `_parseCommands` in src/lib/atemSocket.ts. Unlike upstream this also accepts an
/// 8-byte (empty-bodied) command at the very end, and stops at a length that overruns the payload.
pub fn split_commands(payload: &[u8]) -> Vec<RawCommand<'_>> {
    let mut cmds = Vec::new();
    let mut rest = payload;
    while rest.len() >= COMMAND_HEADER_LEN {
        let len = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
        if len < COMMAND_HEADER_LEN || len > rest.len() {
            break;
        }
        cmds.push(RawCommand { name: [rest[4], rest[5], rest[6], rest[7]], body: &rest[COMMAND_HEADER_LEN..len] });
        rest = &rest[len..];
    }
    cmds
}

/// Wraps a command body in its header.
/// Upstream: `PacketBuilder.addCommand` in src/lib/packetBuilder.ts.
pub fn frame(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let len = (COMMAND_HEADER_LEN + body.len()) as u16;
    let mut v = Vec::with_capacity(usize::from(len));
    v.extend_from_slice(&len.to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(name);
    v.extend_from_slice(body);
    v
}

/// The 4-byte body shared by CPgI and CPvI: u8 M/E, pad, u16 BE source.
fn me_source(mix_effect: u8, source: u16) -> [u8; 4] {
    let s = source.to_be_bytes();
    [mix_effect, 0, s[0], s[1]]
}

// Upstream: src/commands/MixEffects/ProgramInputCommand.ts (`ProgramInputCommand.serialize`).
pub fn cpgi(mix_effect: u8, source: u16) -> Vec<u8> {
    frame(b"CPgI", &me_source(mix_effect, source))
}

// Upstream: src/commands/MixEffects/PreviewInputCommand.ts (`PreviewInputCommand.serialize`).
pub fn cpvi(mix_effect: u8, source: u16) -> Vec<u8> {
    frame(b"CPvI", &me_source(mix_effect, source))
}

// Upstream: src/commands/MixEffects/AutoTransitionCommand.ts (`AutoTransitionCommand.serialize`).
pub fn daut(mix_effect: u8) -> Vec<u8> {
    frame(b"DAut", &[mix_effect, 0, 0, 0])
}

/// `MediaSourceType.Still` in upstream src/enums/index.ts.
pub const MEDIA_SOURCE_STILL: u8 = 1;

/// Mask bits, upstream `MediaPlayerSourceCommand.MaskFlags`.
pub mod mpss_mask {
    pub const SOURCE_TYPE: u8 = 1 << 0;
    pub const STILL_INDEX: u8 = 1 << 1;
    pub const CLIP_INDEX: u8 = 1 << 2;
}

/// MPSS, set a media player's source. `player` is 0-based on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mpss {
    pub mask: u8,
    pub player: u8,
    pub source_type: u8,
    pub still_index: u8,
    pub clip_index: u8,
}

impl Mpss {
    /// Selects still `still_index` on 0-based `player`.
    pub fn still(player: u8, still_index: u8) -> Mpss {
        Mpss {
            mask: mpss_mask::SOURCE_TYPE | mpss_mask::STILL_INDEX,
            player,
            source_type: MEDIA_SOURCE_STILL,
            still_index,
            clip_index: 0,
        }
    }

    // Upstream: src/commands/Media/MediaPlayerSourceCommand.ts (`MediaPlayerSourceCommand.serialize`).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = [0u8; 8];
        b[0] = self.mask;
        b[1] = self.player;
        b[2] = self.source_type;
        b[3] = self.still_index;
        b[4] = self.clip_index;
        frame(b"MPSS", &b)
    }
}

/// Mask bits, upstream `FairlightMixerSourceCommand.MaskFlags`.
pub mod cfsp_mask {
    pub const FRAMES_DELAY: u16 = 1 << 0;
    pub const GAIN: u16 = 1 << 1;
    pub const STEREO_SIMULATION: u16 = 1 << 2;
    pub const EQUALIZER_ENABLED: u16 = 1 << 3;
    pub const EQUALIZER_GAIN: u16 = 1 << 4;
    pub const MAKE_UP_GAIN: u16 = 1 << 5;
    pub const BALANCE: u16 = 1 << 6;
    pub const FADER_GAIN: u16 = 1 << 7;
    pub const MIX_OPTION: u16 = 1 << 8;
}

/// `FairlightAudioMixOption` in upstream src/enums/index.ts.
pub mod mix_option {
    pub const OFF: u8 = 1;
    pub const ON: u8 = 2;
    pub const AUDIO_FOLLOW_VIDEO: u8 = 4;
}

/// CFSP, set Fairlight source properties. Gains are in hundredths of a dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cfsp {
    pub mask: u16,
    pub index: u16,
    pub source: i64,
    pub frames_delay: u8,
    pub gain: i32,
    pub stereo_simulation: i16,
    pub equalizer_enabled: bool,
    pub equalizer_gain: i32,
    pub make_up_gain: i32,
    pub balance: i16,
    pub fader_gain: i32,
    pub mix_option: u8,
}

impl Cfsp {
    /// Sets only the fader gain of one source.
    pub fn fader_gain(index: u16, source: i64, hundredths_db: i32) -> Cfsp {
        Cfsp { mask: cfsp_mask::FADER_GAIN, index, source, fader_gain: hundredths_db, ..Cfsp::default() }
    }

    /// Sets only the mix option (On / Off / AFV) of one source.
    pub fn mix_option(index: u16, source: i64, option: u8) -> Cfsp {
        Cfsp { mask: cfsp_mask::MIX_OPTION, index, source, mix_option: option, ..Cfsp::default() }
    }

    // Upstream: src/commands/Fairlight/FairlightMixerSourceCommand.ts (`FairlightMixerSourceCommand.serialize`).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = [0u8; 48];
        b[0..2].copy_from_slice(&self.mask.to_be_bytes());
        b[2..4].copy_from_slice(&self.index.to_be_bytes());
        b[8..16].copy_from_slice(&self.source.to_be_bytes());
        b[16] = self.frames_delay;
        b[20..24].copy_from_slice(&self.gain.to_be_bytes());
        b[24..26].copy_from_slice(&self.stereo_simulation.to_be_bytes());
        b[26] = u8::from(self.equalizer_enabled);
        b[28..32].copy_from_slice(&self.equalizer_gain.to_be_bytes());
        b[32..36].copy_from_slice(&self.make_up_gain.to_be_bytes());
        b[36..38].copy_from_slice(&self.balance.to_be_bytes());
        b[40..44].copy_from_slice(&self.fader_gain.to_be_bytes());
        b[44] = self.mix_option;
        frame(b"CFSP", &b)
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u8(&self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }
    fn u16(&self, at: usize) -> Option<u16> {
        Some(u16::from_be_bytes(self.0.get(at..at + 2)?.try_into().ok()?))
    }
    fn u32(&self, at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(self.0.get(at..at + 4)?.try_into().ok()?))
    }
    fn i32(&self, at: usize) -> Option<i32> {
        Some(i32::from_be_bytes(self.0.get(at..at + 4)?.try_into().ok()?))
    }
    fn i64(&self, at: usize) -> Option<i64> {
        Some(i64::from_be_bytes(self.0.get(at..at + 8)?.try_into().ok()?))
    }
}

/// A received command StreamIt cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Version(u32),
    Topology {
        mix_effects: u8,
        sources: u8,
        media_players: u8,
    },
    InitComplete,
    ProgramInput {
        mix_effect: u8,
        source: u16,
    },
    PreviewInput {
        mix_effect: u8,
        source: u16,
    },
    TransitionPosition {
        mix_effect: u8,
        in_transition: bool,
        remaining_frames: u8,
        handle_position: u16,
    },
    MediaPlayerSource {
        player: u8,
        source_type: u8,
        still_index: u8,
        clip_index: u8,
    },
    FairlightSource {
        index: u16,
        source: i64,
        fader_gain: i32,
        supported_mix_options: u8,
        mix_option: u8,
    },
    /// A still slot in the media pool (0-based `index`), and its file name while used.
    MediaPoolStill {
        index: u16,
        name: Option<String>,
    },
}

/// Decodes one command, or `None` for one StreamIt ignores (or one too short to read).
pub fn decode(cmd: &RawCommand<'_>) -> Option<Decoded> {
    let r = Reader(cmd.body);
    match &cmd.name {
        // Upstream: src/commands/DeviceProfile/versionCommand.ts (`VersionCommand.deserialize`).
        b"_ver" => Some(Decoded::Version(r.u32(0)?)),
        // Upstream: src/commands/DeviceProfile/topologyCommand.ts (`TopologyCommand.deserialize`).
        // Only the first six bytes are read; they do not move between protocol versions.
        b"_top" => Some(Decoded::Topology { mix_effects: r.u8(0)?, sources: r.u8(1)?, media_players: r.u8(5)? }),
        // Upstream: src/commands/InitCompleteCommand.ts (reads nothing).
        b"InCm" => Some(Decoded::InitComplete),
        // Upstream: src/commands/MixEffects/ProgramInputCommand.ts (`ProgramInputUpdateCommand.deserialize`).
        b"PrgI" => Some(Decoded::ProgramInput { mix_effect: r.u8(0)?, source: r.u16(2)? }),
        // Upstream: src/commands/MixEffects/PreviewInputCommand.ts (`PreviewInputUpdateCommand.deserialize`).
        b"PrvI" => Some(Decoded::PreviewInput { mix_effect: r.u8(0)?, source: r.u16(2)? }),
        // Upstream: src/commands/MixEffects/Transition/TransitionPositionCommand.ts
        // (`TransitionPositionUpdateCommand.deserialize`).
        b"TrPs" => Some(Decoded::TransitionPosition {
            mix_effect: r.u8(0)?,
            in_transition: r.u8(1)? == 1,
            remaining_frames: r.u8(2)?,
            handle_position: r.u16(4)?,
        }),
        // Upstream: src/commands/Media/MediaPoolFrameDescriptionCommand.ts
        // (`MediaPoolFrameDescriptionCommand.deserialize`): bank, index, used, MD5 hash,
        // then the file name's length at 23 and the name from 24. Bank 0 is the stills.
        b"MPfe" if r.u8(0)? == 0 => {
            let index = r.u16(2)?;
            let name = if r.u8(4)? == 1 {
                let len = usize::from(r.u8(23)?);
                let raw = cmd.body.get(24..24 + len).unwrap_or(cmd.body.get(24..).unwrap_or_default());
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                Some(String::from_utf8_lossy(&raw[..end]).trim().to_string())
            } else {
                None
            };
            Some(Decoded::MediaPoolStill { index, name })
        }
        // Upstream: src/commands/Media/MediaPlayerSourceCommand.ts (`MediaPlayerSourceUpdateCommand.deserialize`).
        b"MPCE" => Some(Decoded::MediaPlayerSource {
            player: r.u8(0)?,
            source_type: r.u8(1)?,
            still_index: r.u8(2)?,
            clip_index: r.u8(3)?,
        }),
        // Upstream: src/commands/Fairlight/FairlightMixerSourceCommand.ts
        // (`FairlightMixerSourceUpdateCommand.deserialize`).
        b"FASP" => Some(Decoded::FairlightSource {
            index: r.u16(0)?,
            source: r.i64(8)?,
            fader_gain: r.i32(44)?,
            supported_mix_options: r.u8(48)?,
            mix_option: r.u8(49)?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a libatem-data.json `bytes` string ("00-0C-00-00-...").
    fn hex(s: &str) -> Vec<u8> {
        s.split(['-', ' ', '\n']).filter(|x| !x.is_empty()).map(|x| u8::from_str_radix(x, 16).unwrap()).collect()
    }

    fn decode_bytes(b: &[u8]) -> Option<Decoded> {
        let cmds = split_commands(b);
        assert_eq!(cmds.len(), 1);
        decode(&cmds[0])
    }

    #[test]
    fn encode_cpgi() {
        // libatem-data.json CPgI {"Index": 0, "Source": 4030}
        assert_eq!(cpgi(0, 4030), hex("00-0C-00-00-43-50-67-49-00-00-0F-BE"));
    }

    #[test]
    fn encode_cpvi() {
        // libatem-data.json CPvI {"Index": 0, "Source": 8017}
        assert_eq!(cpvi(0, 8017), hex("00-0C-00-00-43-50-76-49-00-00-1F-51"));
        // CPvI {"Index": 3, "Source": 16}
        assert_eq!(cpvi(3, 16), hex("00-0C-00-00-43-50-76-49-03-00-00-10"));
    }

    #[test]
    fn encode_daut() {
        // libatem-data.json DAut {"Index": 0}
        assert_eq!(daut(0), hex("00-0C-00-00-44-41-75-74-00-00-00-00"));
        assert_eq!(daut(3), hex("00-0C-00-00-44-41-75-74-03-00-00-00"));
    }

    #[test]
    fn encode_mpss() {
        // libatem-data.json MPSS {"Mask": 2, "Index": 2, "SourceType": 2, "StillIndex": 197, "ClipIndex": 214}
        let m = Mpss { mask: 2, player: 2, source_type: 2, still_index: 197, clip_index: 214 };
        assert_eq!(m.encode(), hex("00-10-00-00-4D-50-53-53-02-02-02-C5-D6-00-00-00"));
        // MPSS {"Mask": 1, "Index": 3, "SourceType": 1, "StillIndex": 211, "ClipIndex": 25}
        let m = Mpss { mask: 1, player: 3, source_type: 1, still_index: 211, clip_index: 25 };
        assert_eq!(m.encode(), hex("00-10-00-00-4D-50-53-53-01-03-01-D3-19-00-00-00"));
        // What set_media_player_still sends for player 1 (wire 0), still 5: mask = type | still,
        // as upstream's serializer writes it when only sourceType and stillIndex are set.
        assert_eq!(Mpss::still(0, 5).encode(), hex("00-10-00-00-4D-50-53-53-03-00-01-05-00-00-00-00"));
    }

    #[test]
    fn encode_cfsp_matches_upstream_fixtures() {
        // libatem-data.json CFSP {"Mask": 128, "Index": 11, "SourceId": "532915042168728217",
        // "FramesDelay": 252, "Gain": -41.445…, "StereoSimulation": 5.685…, "EqualizerEnabled": true,
        // "EqualizerGain": -18.338…, "MakeUpGain": 12.098…, "Balance": 36.642…, "FaderGain": -64.610…,
        // "MixOption": 2}; the converters round dB × 100.
        let c = Cfsp {
            mask: 128,
            index: 11,
            source: 532915042168728217,
            frames_delay: 252,
            gain: -4145,
            stereo_simulation: 569,
            equalizer_enabled: true,
            equalizer_gain: -1834,
            make_up_gain: 1210,
            balance: 3664,
            fader_gain: -6461,
            mix_option: 2,
        };
        assert_eq!(
            c.encode(),
            hex(
                "00-38-00-00-43-46-53-50-00-80-00-0B-00-00-00-00-07-65-4B-68-47-ED-EA-99-FC-00-00-00-FF-FF-EF-CF-02-39-01-00-FF-FF-F8-D6-00-00-04-BA-0E-50-00-00-FF-FF-E6-C3-02-00-00-00"
            )
        );
        // CFSP {"Mask": 256, "Index": 4, "SourceId": "-5971177987457376079", "FramesDelay": 62,
        // "Gain": -98.387…, "StereoSimulation": 94.146…, "EqualizerEnabled": false,
        // "EqualizerGain": -16.271…, "MakeUpGain": 19.855…, "Balance": -29.481…,
        // "FaderGain": -73.088…, "MixOption": 2}
        let c = Cfsp {
            mask: 256,
            index: 4,
            source: -5971177987457376079,
            frames_delay: 62,
            gain: -9839,
            stereo_simulation: 9415,
            equalizer_enabled: false,
            equalizer_gain: -1627,
            make_up_gain: 1986,
            balance: -2948,
            fader_gain: -7309,
            mix_option: 2,
        };
        assert_eq!(
            c.encode(),
            hex(
                "00-38-00-00-43-46-53-50-01-00-00-04-00-00-00-00-AD-22-1D-41-CE-F8-A0-B1-3E-00-00-00-FF-FF-D9-91-24-C7-00-00-FF-FF-F9-A5-00-00-07-C2-F4-7C-00-00-FF-FF-E3-73-02-00-00-00"
            )
        );
    }

    #[test]
    fn encode_cfsp_gain_only() {
        // Upstream serializer with only faderGain set: flag 0x0080, index, source, every other
        // field zero, faderGain as i32 BE at body offset 40. Input 1001, source -65280, -10 dB.
        let mut want = hex("00-38-00-00-43-46-53-50-00-80-03-E9-00-00-00-00-FF-FF-FF-FF-FF-FF-01-00");
        want.extend_from_slice(&[0; 24]);
        want.extend_from_slice(&hex("FF-FF-FC-18-00-00-00-00"));
        assert_eq!(want.len(), 56);
        assert_eq!(Cfsp::fader_gain(1001, -65280, -1000).encode(), want);
    }

    #[test]
    fn encode_cfsp_mix_option_only() {
        let mut want = hex("00-38-00-00-43-46-53-50-01-00-03-E9-00-00-00-00-FF-FF-FF-FF-FF-FF-01-00");
        want.extend_from_slice(&[0; 28]);
        want.extend_from_slice(&hex("02-00-00-00"));
        assert_eq!(Cfsp::mix_option(1001, -65280, mix_option::ON).encode(), want);
    }

    #[test]
    fn decode_prgi_prvi() {
        // libatem-data.json PrgI {"Index": 3, "Source": 3020}
        assert_eq!(
            decode_bytes(&hex("00-0C-00-00-50-72-67-49-03-00-0B-CC")),
            Some(Decoded::ProgramInput { mix_effect: 3, source: 3020 })
        );
        // PrvI {"Index": 0, "Source": 8009} (8-byte body on newer protocol versions)
        assert_eq!(
            decode_bytes(&hex("00-10-00-00-50-72-76-49-00-00-1F-49-00-0A-13-01")),
            Some(Decoded::PreviewInput { mix_effect: 0, source: 8009 })
        );
    }

    #[test]
    fn decode_fasp() {
        // libatem-data.json FASP {"Index": 3, "SourceId": "873753620493702708", …,
        // "FaderGain": -79.308…, "SupportedMixOptions": 2, "MixOption": 2}
        let b = hex(
            "00-3C-00-00-46-41-53-50-00-03-00-00-00-00-00-00-0C-20-32-49-DA-BD-16-34-00-A3-CB-00-FF-FF-E1-25-00-00-0E-56-45-00-00-00-FF-FF-FF-77-00-00-05-94-16-41-00-00-FF-FF-E1-05-02-02-00-00",
        );
        assert_eq!(
            decode_bytes(&b),
            Some(Decoded::FairlightSource {
                index: 3,
                source: 873753620493702708,
                fader_gain: -7931,
                supported_mix_options: 2,
                mix_option: 2,
            })
        );
        // FASP {"Index": 1101, "SourceId": "-451415100190981249", "FaderGain": -29.452…,
        // "SupportedMixOptions": 1, "MixOption": 4}
        let b = hex(
            "00-3C-00-00-46-41-53-50-04-4D-00-00-00-00-00-00-F9-BC-40-5B-AA-DE-83-7F-01-18-D4-00-FF-FF-DB-0B-00-00-06-08-21-00-00-00-FF-FF-FE-2C-00-00-03-14-FA-12-00-00-FF-FF-F4-7F-01-04-00-00",
        );
        assert_eq!(
            decode_bytes(&b),
            Some(Decoded::FairlightSource {
                index: 1101,
                source: -451415100190981249,
                fader_gain: -2945,
                supported_mix_options: 1,
                mix_option: 4,
            })
        );
    }

    #[test]
    fn decode_trps() {
        // libatem-data.json TrPs {"Index": 0, "InTransition": true, "RemainingFrames": 249, "HandlePosition": 0.618…}
        assert_eq!(
            decode_bytes(&hex("00-10-00-00-54-72-50-73-00-01-F9-00-18-28-00-00")),
            Some(Decoded::TransitionPosition {
                mix_effect: 0,
                in_transition: true,
                remaining_frames: 249,
                handle_position: 6184,
            })
        );
        // TrPs {"Index": 1, "InTransition": false, "RemainingFrames": 204, …}
        assert_eq!(
            decode_bytes(&hex("00-10-00-00-54-72-50-73-01-00-CC-00-02-B6-00-00")),
            Some(Decoded::TransitionPosition {
                mix_effect: 1,
                in_transition: false,
                remaining_frames: 204,
                handle_position: 694,
            })
        );
    }

    #[test]
    fn decode_mpfe_names_the_still_slots() {
        // Bank 0 (stills), index 4, used, a 16-byte hash, the name's length at 23.
        let mut body = vec![0, 0, 0, 4, 1];
        body.extend([0xAB; 16]);
        body.extend([0, 0, 13]);
        body.extend(b"Notices.png  ");
        assert_eq!(
            decode_bytes(&frame(b"MPfe", &body)),
            Some(Decoded::MediaPoolStill { index: 4, name: Some("Notices.png".into()) })
        );
        // An empty slot has no name.
        let mut empty = vec![0, 0, 0, 5, 0];
        empty.extend([0; 19]);
        assert_eq!(decode_bytes(&frame(b"MPfe", &empty)), Some(Decoded::MediaPoolStill { index: 5, name: None }));
        // Bank 1 is clips: not stills.
        let mut clip = body.clone();
        clip[0] = 1;
        assert_eq!(decode_bytes(&frame(b"MPfe", &clip)), None);
    }

    #[test]
    fn decode_mpce_ver_top_incm() {
        // libatem-data.json MPCE {"Index": 0, "SourceType": 1, "SourceIndex": 57}
        assert_eq!(
            decode_bytes(&hex("00-0C-00-00-4D-50-43-45-00-01-39-00")),
            Some(Decoded::MediaPlayerSource { player: 0, source_type: 1, still_index: 57, clip_index: 0 })
        );
        // _ver {"ProtocolVersion": 131100}
        assert_eq!(decode_bytes(&hex("00-0C-00-00-5F-76-65-72-00-02-00-1C")), Some(Decoded::Version(131100)));
        // _top {"MixEffectBlocks": 71, "VideoSources": 49, …, "MediaPlayers": 190, …}
        assert_eq!(
            decode_bytes(&hex("00-1C-00-00-5F-74-6F-70-47-31-0F-C4-D6-BE-AB-16-80-3A-D9-00-00-00-00-00-00-00-00-00")),
            Some(Decoded::Topology { mix_effects: 71, sources: 49, media_players: 190 })
        );
        // InCm has no fixture upstream (its deserializer reads nothing); TODO(capture).
        assert_eq!(decode_bytes(&hex("00-0C-00-00-49-6E-43-6D-01-00-00-00")), Some(Decoded::InitComplete));
    }

    #[test]
    fn ignores_unknown_and_short_commands() {
        assert_eq!(decode_bytes(&hex("00-0C-00-00-58-58-58-58-00-00-00-00")), None);
        // A PrgI too short to hold a source is ignored, not misread.
        assert_eq!(decode_bytes(&hex("00-0A-00-00-50-72-67-49-00-00")), None);
    }

    #[test]
    fn splits_a_payload_into_commands() {
        let mut p = cpvi(0, 2);
        p.extend(hex("00-08-00-00-49-6E-43-6D"));
        let cmds = split_commands(&p);
        assert_eq!(cmds.len(), 2);
        assert_eq!(&cmds[0].name, b"CPvI");
        assert_eq!(cmds[0].body, &[0, 0, 0, 2]);
        assert_eq!(&cmds[1].name, b"InCm");
        assert!(cmds[1].body.is_empty());
        // A length that overruns the payload, or is shorter than a header, stops the split.
        let mut p = cpvi(0, 2);
        p.extend(hex("00-20-00-00-50-72-67-49-00-00"));
        assert_eq!(split_commands(&p).len(), 1);
        assert!(split_commands(&hex("00-04-00-00-50-72-67-49")).is_empty());
    }
}
