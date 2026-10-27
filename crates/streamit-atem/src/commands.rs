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
    /// The switcher's video mode (upstream `Enums.VideoMode`).
    VideoMode(u8),
    /// What input `input` is called, and whether that is the switcher's own name for it.
    InputName {
        input: u16,
        long: String,
        short: String,
        default: bool,
    },
    /// Our lock on a media store (0: the stills) is granted.
    LockObtained(u16),
    LockState {
        index: u16,
        locked: bool,
    },
    /// A chunk of a still being downloaded.
    TransferData {
        id: u16,
        body: Vec<u8>,
    },
    TransferComplete(u16),
    TransferError {
        id: u16,
        code: u8,
    },
    /// The switcher wants `chunk_count` more chunks of at most `chunk_size` bytes.
    TransferContinue {
        id: u16,
        chunk_size: u16,
        chunk_count: u16,
    },
}

impl Decoded {
    /// Part of a data transfer (or its lock): routed to the transfer under way, not state.
    pub fn is_transfer(&self) -> bool {
        matches!(
            self,
            Decoded::LockObtained(_)
                | Decoded::LockState { .. }
                | Decoded::TransferData { .. }
                | Decoded::TransferComplete(_)
                | Decoded::TransferError { .. }
                | Decoded::TransferContinue { .. }
        )
    }
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
        // Upstream: src/commands/Settings/VideoMode.ts (`VideoModeUpdateCommand.deserialize`).
        b"VidM" => Some(Decoded::VideoMode(r.u8(0)?)),
        // Upstream: src/commands/Inputs/InputPropertiesCommand.ts (`InputPropertiesUpdateCommand.deserialize`):
        // the input, its long name in 20 bytes from 2, its short name in 4 from 22, then
        // whether both are the switcher's own at 26.
        b"InPr" => Some(Decoded::InputName {
            input: r.u16(0)?,
            long: c_string(cmd.body.get(2..22)?),
            short: c_string(cmd.body.get(22..26)?),
            default: r.u8(26)? == 1,
        }),
        // Upstream: src/commands/DataTransfer/LockObtainedCommand.ts.
        b"LKOB" => Some(Decoded::LockObtained(r.u16(0)?)),
        // Upstream: src/commands/DataTransfer/LockStateCommand.ts (`LockStateUpdateCommand`).
        b"LKST" => Some(Decoded::LockState { index: r.u16(0)?, locked: r.u8(2)? == 1 }),
        // Upstream: src/commands/DataTransfer/DataTransferDataCommand.ts: id, size, then the data.
        b"FTDa" => {
            let size = usize::from(r.u16(2)?);
            Some(Decoded::TransferData { id: r.u16(0)?, body: cmd.body.get(4..4 + size)?.to_vec() })
        }
        // Upstream: src/commands/DataTransfer/DataTransferCompleteCommand.ts.
        b"FTDC" => Some(Decoded::TransferComplete(r.u16(0)?)),
        // Upstream: src/commands/DataTransfer/DataTransferErrorCommand.ts.
        b"FTDE" => Some(Decoded::TransferError { id: r.u16(0)?, code: r.u8(2)? }),
        // Upstream: src/commands/DataTransfer/DataTransferUploadContinueCommand.ts.
        b"FTCD" => Some(Decoded::TransferContinue { id: r.u16(0)?, chunk_size: r.u16(6)?, chunk_count: r.u16(8)? }),
        _ => None,
    }
}

// Upstream: src/commands/DataTransfer/LockStateCommand.ts (`LockStateCommand.serialize`).
pub fn lock(store: u16, locked: bool) -> Vec<u8> {
    let s = store.to_be_bytes();
    frame(b"LOCK", &[s[0], s[1], u8::from(locked), 0])
}

// Upstream: src/commands/DataTransfer/DataTransferDownloadRequestCommand.ts; a still is
// type 0x00f9 (src/dataTransfer/dataTransferDownloadStill.ts).
pub fn ftsu(id: u16, store: u16, index: u16) -> Vec<u8> {
    let mut b = [0u8; 12];
    b[0..2].copy_from_slice(&id.to_be_bytes());
    b[2..4].copy_from_slice(&store.to_be_bytes());
    b[6..8].copy_from_slice(&index.to_be_bytes());
    b[8..10].copy_from_slice(&0x00f9u16.to_be_bytes());
    frame(b"FTSU", &b)
}

// Upstream: src/commands/DataTransfer/DataTransferUploadRequestCommand.ts; a still is mode 1
// (src/dataTransfer/dataTransferUploadStill.ts). `size`: the length before RLE.
pub fn ftsd(id: u16, store: u16, index: u16, size: u32) -> Vec<u8> {
    let mut b = [0u8; 16];
    b[0..2].copy_from_slice(&id.to_be_bytes());
    b[2..4].copy_from_slice(&store.to_be_bytes());
    b[6..8].copy_from_slice(&index.to_be_bytes());
    b[8..12].copy_from_slice(&size.to_be_bytes());
    b[12..14].copy_from_slice(&1u16.to_be_bytes());
    frame(b"FTSD", &b)
}

// Upstream: src/commands/DataTransfer/DataTransferFileDescriptionCommand.ts: name in 64
// bytes, description in 128, then the 16-byte MD5 of the data before RLE.
pub fn ftfd(id: u16, name: &str, description: &str, hash: &[u8; 16]) -> Vec<u8> {
    let mut b = [0u8; 212];
    b[0..2].copy_from_slice(&id.to_be_bytes());
    let put = |b: &mut [u8], s: &str| {
        let n = s.len().min(b.len());
        b[..n].copy_from_slice(&s.as_bytes()[..n]);
    };
    put(&mut b[2..66], name);
    put(&mut b[66..194], description);
    b[194..210].copy_from_slice(hash);
    frame(b"FTFD", &b)
}

// Upstream: src/commands/DataTransfer/DataTransferAckCommand.ts.
pub fn ftua(id: u16, index: u8) -> Vec<u8> {
    let i = id.to_be_bytes();
    frame(b"FTUA", &[i[0], i[1], index, 0])
}

// Upstream: src/commands/DataTransfer/DataTransferDataCommand.ts (`serialize`).
pub fn ftda(id: u16, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + data.len());
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(&(data.len() as u16).to_be_bytes());
    b.extend_from_slice(data);
    frame(b"FTDa", &b)
}

/// Text up to its first NUL.
fn c_string(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

/// Copies `s` into `b`, cut short at a character boundary to fit.
fn put_text(b: &mut [u8], s: &str) {
    let mut n = s.len().min(b.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
}

// Upstream: src/commands/Inputs/InputPropertiesCommand.ts (`InputPropertiesCommand.serialize`):
// the mask (1 long name, 2 short name), the input at 2, the long name in 20 bytes from 4,
// the short name in 4 from 24.
pub fn cinl(input: u16, long: &str, short: &str) -> Vec<u8> {
    let mut b = [0u8; 32];
    b[0] = 1 | 2;
    b[2..4].copy_from_slice(&input.to_be_bytes());
    put_text(&mut b[4..24], long);
    put_text(&mut b[24..28], short);
    frame(b"CInL", &b)
}

// Upstream: src/commands/Media/MediaPoolClearStillCommand.ts.
pub fn cstl(index: u8) -> Vec<u8> {
    frame(b"CSTL", &[index, 0, 0, 0])
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
    fn transfer_commands_follow_upstream_layouts() {
        assert_eq!(lock(0, true), [0, 12, 0, 0, b'L', b'O', b'C', b'K', 0, 0, 1, 0]);
        assert_eq!(ftsu(3, 0, 5)[8..], [0, 3, 0, 0, 0, 0, 0, 5, 0, 0xf9, 0, 0]);
        let sd = ftsd(3, 0, 5, 1920 * 1080 * 4);
        assert_eq!(sd[8..], [0, 3, 0, 0, 0, 0, 0, 5, 0, 0x7e, 0x90, 0, 0, 1, 0, 0]);
        let fd = ftfd(3, "Notices", "", &[7; 16]);
        assert_eq!(fd.len(), 8 + 212);
        assert_eq!(&fd[10..17], b"Notices");
        assert_eq!(fd[8 + 194..8 + 210], [7; 16]);
        assert_eq!(ftua(3, 5)[8..], [0, 3, 5, 0]);
        assert_eq!(ftda(3, &[9, 9])[8..], [0, 3, 0, 2, 9, 9]);
        assert_eq!(cstl(4)[4..], [b'C', b'S', b'T', b'L', 4, 0, 0, 0]);
    }

    #[test]
    fn transfer_replies_decode() {
        assert_eq!(decode_bytes(&frame(b"LKOB", &[0, 0, 0, 0])), Some(Decoded::LockObtained(0)));
        assert_eq!(decode_bytes(&frame(b"LKST", &[0, 0, 1, 0])), Some(Decoded::LockState { index: 0, locked: true }));
        assert_eq!(
            decode_bytes(&frame(b"FTDa", &[0, 3, 0, 2, 9, 8, 0, 0])),
            Some(Decoded::TransferData { id: 3, body: vec![9, 8] })
        );
        assert_eq!(decode_bytes(&frame(b"FTDC", &[0, 3, 0, 0])), Some(Decoded::TransferComplete(3)));
        assert_eq!(decode_bytes(&frame(b"FTDE", &[0, 3, 1, 0])), Some(Decoded::TransferError { id: 3, code: 1 }));
        assert_eq!(
            decode_bytes(&frame(b"FTCD", &[0, 3, 0, 0, 0, 0, 5, 0x6c, 0, 20, 0, 0])),
            Some(Decoded::TransferContinue { id: 3, chunk_size: 1388, chunk_count: 20 })
        );
        assert_eq!(decode_bytes(&frame(b"VidM", &[11, 0, 0, 0])), Some(Decoded::VideoMode(11)));
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
    fn decode_inpr_names_an_input() {
        let mut body = vec![0, 3];
        let mut long = [0u8; 20];
        long[..10].copy_from_slice(b"Pulpit Cam");
        body.extend(long);
        body.extend(b"PULP");
        body.extend([0, 0]);
        body.extend([0; 10]);
        assert_eq!(
            decode_bytes(&frame(b"InPr", &body)),
            Some(Decoded::InputName { input: 3, long: "Pulpit Cam".into(), short: "PULP".into(), default: false })
        );
        body[26] = 1;
        assert!(matches!(decode_bytes(&frame(b"InPr", &body)), Some(Decoded::InputName { default: true, .. })));
    }

    #[test]
    fn cinl_sets_both_names() {
        let f = cinl(3, "Pulpit Cam", "PULP");
        let b = &f[8..];
        assert_eq!(&f[4..8], b"CInL");
        assert_eq!(b.len(), 32);
        assert_eq!((b[0], &b[2..4]), (3, &[0u8, 3][..]));
        assert_eq!(&b[4..14], b"Pulpit Cam");
        assert_eq!(b[14], 0);
        assert_eq!(&b[24..28], b"PULP");
        // Too long: cut to the field, never past a character.
        let f = cinl(1, "Überlange Kamera Nummer eins", "ÜBER");
        assert_eq!(c_string(&f[8 + 4..8 + 24]), "Überlange Kamera Nu");
        assert_eq!(c_string(&f[8 + 24..8 + 28]), "ÜBE");
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
