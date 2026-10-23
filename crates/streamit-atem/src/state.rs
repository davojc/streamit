//! The slice of switcher state StreamIt reads, built from decoded commands.
use crate::commands::{Decoded, decode, split_commands};
use std::collections::BTreeMap;
use streamit_core::config::AudioSourceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaPlayer {
    pub source_type: u8,
    pub still_index: u8,
    pub clip_index: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FairlightSource {
    /// Hundredths of a dB.
    pub fader_gain: i32,
    pub mix_option: u8,
    pub supported_mix_options: u8,
}

/// State of one session. A fresh session starts from `AtemState::default()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AtemState {
    pub protocol_version: Option<u32>,
    pub mix_effects: Option<u8>,
    pub media_player_count: Option<u8>,
    /// `InCm` received: the initial state dump is complete.
    pub initialised: bool,
    pub program: BTreeMap<u8, u16>,
    pub preview: BTreeMap<u8, u16>,
    pub in_transition: BTreeMap<u8, bool>,
    /// By 0-based player.
    pub media_players: BTreeMap<u8, MediaPlayer>,
    /// By (input index, source id).
    pub fairlight: BTreeMap<(u16, i64), FairlightSource>,
}

impl AtemState {
    pub fn apply(&mut self, d: &Decoded) {
        match *d {
            Decoded::Version(v) => self.protocol_version = Some(v),
            Decoded::Topology {
                mix_effects,
                media_players,
                ..
            } => {
                self.mix_effects = Some(mix_effects);
                self.media_player_count = Some(media_players);
            }
            Decoded::InitComplete => self.initialised = true,
            Decoded::ProgramInput { mix_effect, source } => {
                self.program.insert(mix_effect, source);
            }
            Decoded::PreviewInput { mix_effect, source } => {
                self.preview.insert(mix_effect, source);
            }
            Decoded::TransitionPosition {
                mix_effect,
                in_transition,
                ..
            } => {
                self.in_transition.insert(mix_effect, in_transition);
            }
            Decoded::MediaPlayerSource {
                player,
                source_type,
                still_index,
                clip_index,
            } => {
                self.media_players.insert(
                    player,
                    MediaPlayer {
                        source_type,
                        still_index,
                        clip_index,
                    },
                );
            }
            Decoded::FairlightSource {
                index,
                source,
                fader_gain,
                supported_mix_options,
                mix_option,
            } => {
                self.fairlight.insert(
                    (index, source),
                    FairlightSource {
                        fader_gain,
                        mix_option,
                        supported_mix_options,
                    },
                );
            }
        }
    }

    /// Applies every command in a packet payload; unknown commands are ignored.
    pub fn apply_payload(&mut self, payload: &[u8]) {
        for cmd in split_commands(payload) {
            if let Some(d) = decode(&cmd) {
                self.apply(&d);
            }
        }
    }

    /// Fader gain in dB of one Fairlight source, if the ATEM has reported it.
    pub fn fader_db(&self, src: AudioSourceId) -> Option<f32> {
        self.fairlight
            .get(&(src.input, src.source))
            .map(|f| f.fader_gain as f32 / 100.0)
    }

    pub fn has_fairlight(&self) -> bool {
        !self.fairlight.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::frame;

    #[test]
    fn program_updates_from_prgi() {
        let mut s = AtemState::default();
        let mut p = frame(b"PrgI", &[0, 0, 0x0b, 0xc4]); // M/E 0 <- 3012
        p.extend(frame(b"PrvI", &[0, 0, 0, 2, 0, 0, 0, 0]));
        p.extend(frame(b"PrgI", &[1, 0, 0, 7])); // M/E 1 is tracked separately
        s.apply_payload(&p);
        assert_eq!(s.program.get(&0), Some(&3012));
        assert_eq!(s.program.get(&1), Some(&7));
        assert_eq!(s.preview.get(&0), Some(&2));
        assert!(!s.initialised);

        s.apply_payload(&frame(b"PrgI", &[0, 0, 0, 1]));
        assert_eq!(s.program.get(&0), Some(&1));
    }

    #[test]
    fn init_complete_and_topology() {
        let mut s = AtemState::default();
        let mut p = frame(b"_ver", &[0, 2, 0, 0x1e]);
        p.extend(frame(b"_top", &[1, 40, 2, 1, 0, 2, 0, 0, 0, 0, 0, 0]));
        p.extend(frame(b"XXXX", &[1, 2, 3, 4]));
        p.extend(frame(b"InCm", &[1, 0, 0, 0]));
        s.apply_payload(&p);
        assert_eq!(s.protocol_version, Some(0x0002_001e));
        assert_eq!(s.mix_effects, Some(1));
        assert_eq!(s.media_player_count, Some(2));
        assert!(s.initialised);
    }

    #[test]
    fn level_reads_from_fasp() {
        let mut s = AtemState::default();
        let src = AudioSourceId {
            input: 1001,
            source: -65280,
        };
        assert_eq!(s.fader_db(src), None);
        assert!(!s.has_fairlight());

        // FASP body: index 1001, source -65280, fader gain -12.5 dB, supported On|Off, mix On.
        let mut body = [0u8; 52];
        body[0..2].copy_from_slice(&1001u16.to_be_bytes());
        body[8..16].copy_from_slice(&(-65280i64).to_be_bytes());
        body[44..48].copy_from_slice(&(-1250i32).to_be_bytes());
        body[48] = 3;
        body[49] = 2;
        s.apply_payload(&frame(b"FASP", &body));
        assert!(s.has_fairlight());
        assert_eq!(s.fader_db(src), Some(-12.5));
        assert_eq!(
            s.fairlight.get(&(1001, -65280)).map(|f| f.mix_option),
            Some(2)
        );
        assert_eq!(
            s.fader_db(AudioSourceId {
                input: 1001,
                source: -255
            }),
            None
        );
    }

    #[test]
    fn transition_and_media_player() {
        let mut s = AtemState::default();
        s.apply_payload(&frame(b"TrPs", &[0, 1, 20, 0, 0x10, 0, 0, 0]));
        assert_eq!(s.in_transition.get(&0), Some(&true));
        s.apply_payload(&frame(b"TrPs", &[0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(s.in_transition.get(&0), Some(&false));
        s.apply_payload(&frame(b"MPCE", &[1, 1, 4, 0]));
        assert_eq!(
            s.media_players.get(&1),
            Some(&MediaPlayer {
                source_type: 1,
                still_index: 4,
                clip_index: 0
            })
        );
    }
}
