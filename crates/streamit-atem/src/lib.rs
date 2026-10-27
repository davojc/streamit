//! Blackmagic ATEM client (UDP 9910): switching, media player stills and Fairlight levels.
//!
//! The wire format is ported from atem-connection (MIT, github.com/nrkno/sofie-atem-connection,
//! `main` at 9c77b69919d10108c92231903023acf4cf1cf7db). `packet` is the session layer, `commands`
//! the byte layouts, `state` the slice of switcher state StreamIt reads.
//!
//! Numbering: inputs and Fairlight ids are the ATEM's own. Media players are 1-based here (as in
//! ATEM Software Control and `media_player_input`) and 0-based on the wire. Only M/E 0 is used.
pub mod commands;
pub mod packet;
pub mod probe;
pub mod state;
pub mod still;

use async_trait::async_trait;
use commands::{Cfsp, Mpss, cpgi, cpvi, daut, mix_option};
use packet::{LinkEvent, SendRequest, Timings, Transport, UdpTransport, run_link};
use state::AtemState;
use std::net::SocketAddr;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{AudioInput, AudioMix, DeviceError, Result, Still, StreamSwitcher, Switcher};
use tokio::sync::{mpsc, oneshot, watch};

pub use packet::ATEM_PORT;

/// The Fairlight fader floor (−∞ on the console); anything at or below it is sent as this.
pub const FADER_FLOOR_DB: f32 = -100.0;
/// The Fairlight fader maximum.
pub const FADER_MAX_DB: f32 = 10.0;

const ME: u8 = 0;

/// What the link task publishes.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// A session is established (hello answered and not lost since).
    pub connected: bool,
    /// Counts sessions, so a wait can tell a reconnect from a state change.
    pub session: u64,
    /// Counts TrPs updates for M/E 0 in this session.
    pub transition_updates: u64,
    pub state: AtemState,
}

pub struct AtemClient {
    shared: watch::Receiver<Snapshot>,
    requests: mpsc::Sender<SendRequest>,
    /// (session, transition_updates) when the last auto was sent; see `wait_transition_done`.
    auto_mark: Mutex<Option<(u64, u64)>>,
    /// Where transfer replies go while a transfer runs (one at a time: `transfer_turn`).
    transfer: Arc<Mutex<Option<mpsc::UnboundedSender<commands::Decoded>>>>,
    transfer_turn: tokio::sync::Mutex<()>,
    next_transfer: AtomicU16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AtemClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl AtemClient {
    /// Starts the client. It does not wait for the switcher: until the session is up and the
    /// initial state dump (ending in `InCm`) has arrived, every call reports `Disconnected`,
    /// and it keeps retrying in the background.
    pub async fn connect(addr: SocketAddr) -> Result<Arc<AtemClient>> {
        let t = UdpTransport::bind(addr)
            .await
            .map_err(|e| DeviceError::Other(format!("cannot open udp socket for {addr}: {e}")))?;
        Ok(Self::with_transport(t, Timings::default()))
    }

    /// Starts the client over any transport (tests use an in-memory one).
    pub fn with_transport<T: Transport>(transport: T, timings: Timings) -> Arc<AtemClient> {
        let (tx, rx) = watch::channel(Snapshot::default());
        let (req_tx, req_rx) = mpsc::channel(32);
        let transfer: Arc<Mutex<Option<mpsc::UnboundedSender<commands::Decoded>>>> = Arc::default();
        let to_transfer = transfer.clone();
        let on_event = move |e: LinkEvent| match e {
            LinkEvent::Connected => tx.send_modify(|s| {
                s.connected = true;
                s.session += 1;
                s.transition_updates = 0;
                s.state = AtemState::default();
            }),
            LinkEvent::Lost => tx.send_modify(|s| {
                s.connected = false;
                s.state = AtemState::default();
            }),
            LinkEvent::Commands(p) => {
                let (moves, state): (Vec<_>, Vec<_>) = commands::split_commands(&p)
                    .iter()
                    .filter_map(commands::decode)
                    .partition(commands::Decoded::is_transfer);
                if !moves.is_empty()
                    && let Some(t) = to_transfer.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                {
                    for d in moves {
                        let _ = t.send(d);
                    }
                }
                // A packet of transfer data alone changes nothing watchers care about.
                if !state.is_empty() {
                    tx.send_modify(|s| {
                        for d in &state {
                            if matches!(d, commands::Decoded::TransitionPosition { mix_effect: ME, .. }) {
                                s.transition_updates += 1;
                            }
                            s.state.apply(d);
                        }
                    });
                }
            }
        };
        let task = tokio::spawn(run_link(transport, timings, req_rx, on_event));
        Arc::new(AtemClient {
            shared: rx,
            requests: req_tx,
            auto_mark: Mutex::new(None),
            transfer,
            transfer_turn: tokio::sync::Mutex::new(()),
            next_transfer: AtomicU16::new(1),
            task,
        })
    }

    /// A copy of the current state, initialised or not.
    pub fn snapshot(&self) -> Snapshot {
        self.shared.borrow().clone()
    }

    /// Waits until the initial state dump has arrived.
    pub async fn wait_initialised(&self, timeout: Duration) -> Result<()> {
        let mut rx = self.shared.clone();
        let wait = async {
            loop {
                if ready(&rx.borrow_and_update()).is_ok() {
                    return Ok(());
                }
                if rx.changed().await.is_err() {
                    return Err(stopped());
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or_else(|_| Err(DeviceError::Timeout("atem state not received".into())))
    }

    /// Reads the initialised state, or says why it cannot.
    fn read<R>(&self, f: impl FnOnce(&AtemState) -> Result<R>) -> Result<R> {
        let s = self.shared.borrow();
        ready(&s)?;
        f(&s.state)
    }

    /// Sends one command in its own reliable packet and waits for the ATEM's ack.
    async fn send(&self, payload: Vec<u8>) -> Result<()> {
        let (done, rx) = oneshot::channel();
        self.requests.send(SendRequest { payload, done }).await.map_err(|_| stopped())?;
        rx.await.map_err(|_| stopped())?
    }

    /// Checks a Fairlight source exists and returns its state.
    fn fairlight(&self, src: AudioSourceId) -> Result<state::FairlightSource> {
        self.read(|s| {
            if !s.has_fairlight() {
                return Err(DeviceError::Refused("no fairlight".into()));
            }
            s.fairlight.get(&(src.input, src.source)).copied().ok_or_else(|| {
                DeviceError::Refused(format!("no fairlight source {} / {} on this atem", src.input, src.source))
            })
        })
    }
}

fn ready(s: &Snapshot) -> Result<()> {
    if !s.connected {
        return Err(DeviceError::Disconnected("atem not connected".into()));
    }
    if !s.state.initialised {
        return Err(DeviceError::Disconnected("atem state not yet received".into()));
    }
    Ok(())
}

fn stopped() -> DeviceError {
    DeviceError::Disconnected("atem client stopped".into())
}

/// dB to the CFSP fader gain in hundredths: at or below the floor (including −∞) is the floor.
fn fader_hundredths(db: f32) -> Result<i32> {
    if db.is_nan() {
        return Err(DeviceError::Refused("fader gain is not a number".into()));
    }
    if db > FADER_MAX_DB {
        return Err(DeviceError::Refused(format!("fader gain {db} dB is above +10 dB")));
    }
    let db = db.max(FADER_FLOOR_DB);
    Ok((db * 100.0).round() as i32)
}

#[async_trait]
impl Switcher for AtemClient {
    fn stills(&self) -> Vec<Option<String>> {
        let s = self.shared.borrow();
        let len = s.state.stills.keys().next_back().map_or(0, |&i| usize::from(i) + 1);
        (0..len).map(|i| s.state.stills.get(&(i as u16)).cloned().flatten()).collect()
    }

    fn still_size(&self) -> Option<(u32, u32)> {
        self.shared.borrow().state.video_mode.and_then(still::mode_size)
    }

    async fn download_still(&self, slot: u16) -> Result<Still> {
        let (width, height) = self.size_for_stills()?;
        let index = u8::try_from(slot).map_err(|_| DeviceError::Other(format!("no media slot {}", slot + 1)))?;
        let raw = self.download(index).await?;
        let yuv = still::decode_rle(&raw, (width * height * 4) as usize);
        Ok(Still { width, height, rgba: still::yuv422_to_rgba(width, height, &yuv) })
    }

    async fn upload_still(&self, slot: u16, picture: Still, name: &str) -> Result<()> {
        let (width, height) = self.size_for_stills()?;
        if (picture.width, picture.height) != (width, height) {
            return Err(DeviceError::Other(format!(
                "the picture is {}x{}; the switcher's stills are {width}x{height}",
                picture.width, picture.height
            )));
        }
        let yuv = still::rgba_to_yuv422(width, height, &picture.rgba);
        let hash: [u8; 16] = {
            use md5::Digest;
            md5::Md5::digest(&yuv).into()
        };
        let data = still::encode_rle(&yuv);
        let size = yuv.len() as u32;
        self.upload(slot, size, &data, name, &hash).await
    }

    async fn clear_still(&self, slot: u16) -> Result<()> {
        self.read(|_| Ok(()))?;
        let index = u8::try_from(slot).map_err(|_| DeviceError::Other(format!("no media slot {}", slot + 1)))?;
        self.send(commands::cstl(index)).await
    }

    async fn program(&self) -> Result<u16> {
        self.read(|s| {
            s.program.get(&ME).copied().ok_or_else(|| DeviceError::Other("atem reported no program input".into()))
        })
    }

    async fn preview(&self) -> Result<u16> {
        self.read(|s| {
            s.preview.get(&ME).copied().ok_or_else(|| DeviceError::Other("atem reported no preview input".into()))
        })
    }

    async fn set_preview(&self, input: u16) -> Result<()> {
        self.read(|_| Ok(()))?;
        self.send(cpvi(ME, input)).await
    }

    async fn set_program(&self, input: u16) -> Result<()> {
        self.read(|_| Ok(()))?;
        self.send(cpgi(ME, input)).await
    }

    async fn auto(&self) -> Result<()> {
        let mark = {
            let s = self.shared.borrow();
            ready(&s)?;
            (s.session, s.transition_updates)
        };
        // Taken before sending: the TrPs that starts the transition may beat the ack.
        *self.auto_mark.lock().unwrap_or_else(|e| e.into_inner()) = Some(mark);
        self.send(daut(ME)).await
    }

    /// Resolves once M/E 0 is not in a transition. After an `auto`, the transition counts as
    /// pending until the switcher has reported a transition position since the auto was sent,
    /// so a stale "idle" from before the auto does not end the wait.
    async fn wait_transition_done(&self, timeout: Duration) -> Result<()> {
        let mark = *self.auto_mark.lock().unwrap_or_else(|e| e.into_inner());
        let mut rx = self.shared.clone();
        let wait = async {
            loop {
                {
                    let s = rx.borrow_and_update();
                    ready(&s)?;
                    let auto_pending = mark == Some((s.session, s.transition_updates));
                    let in_transition = s.state.in_transition.get(&ME).copied().unwrap_or(false);
                    if !auto_pending && !in_transition {
                        return Ok(());
                    }
                }
                if rx.changed().await.is_err() {
                    return Err(stopped());
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or_else(|_| Err(DeviceError::Timeout("atem transition still running".into())))
    }
}

#[async_trait]
impl StreamSwitcher for AtemClient {
    async fn set_media_player_still(&self, player: u8, still: u16) -> Result<()> {
        let count = self.read(|s| Ok(s.media_player_count.unwrap_or(0)))?;
        if player == 0 || player > count {
            return Err(DeviceError::Refused(format!("media player {player} does not exist (this atem has {count})")));
        }
        let still =
            u8::try_from(still).map_err(|_| DeviceError::Refused(format!("still index {still} is out of range")))?;
        self.send(Mpss::still(player - 1, still).encode()).await
    }

    fn media_player_input(&self, player: u8) -> u16 {
        3000 + 10 * u16::from(player)
    }

    async fn audio_level(&self, src: AudioSourceId) -> Result<f32> {
        Ok(self.fairlight(src)?.fader_gain as f32 / 100.0)
    }

    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()> {
        self.fairlight(src)?;
        let hundredths = fader_hundredths(db)?;
        self.send(Cfsp::fader_gain(src.input, src.source, hundredths).encode()).await
    }

    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()> {
        let f = self.fairlight(src)?;
        let option = if on { mix_option::ON } else { mix_option::OFF };
        if f.supported_mix_options & option == 0 {
            return Err(DeviceError::Refused(format!(
                "fairlight source {} / {} cannot be switched {}",
                src.input,
                src.source,
                if on { "on" } else { "off" }
            )));
        }
        self.send(Cfsp::mix_option(src.input, src.source, option).encode()).await
    }

    async fn audio_input(&self, src: AudioSourceId) -> Result<AudioInput> {
        let f = self.fairlight(src)?;
        let mix = match f.mix_option {
            mix_option::ON => AudioMix::On,
            mix_option::AUDIO_FOLLOW_VIDEO => AudioMix::FollowVideo,
            _ => AudioMix::Off,
        };
        Ok(AudioInput { mix, fader_db: f.fader_gain as f32 / 100.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::commands::frame;
    use super::packet::fake::*;
    use super::packet::{HEADER_LEN, Header, flag};
    use super::*;
    use tokio::time::Instant;

    const SESSION: u16 = 0x8001;
    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };

    /// A fake switcher that has answered the hello; sends numbered reliable packets.
    struct Atem {
        io: FakeAtem,
        next_id: u16,
    }

    impl Atem {
        async fn start() -> (Atem, Arc<AtemClient>) {
            let (t, mut io) = pair();
            let client = AtemClient::with_transport(t, Timings::default());
            io.accept().await;
            (Atem { io, next_id: 1 }, client)
        }

        fn push(&mut self, payload: &[u8]) {
            self.io.to_client.send(atem_packet(SESSION, self.next_id, payload)).unwrap();
            self.next_id += 1;
        }

        fn dump(&mut self, fairlight: bool) {
            let mut p = frame(b"_ver", &[0, 2, 0, 0x1e]);
            p.extend(frame(b"_top", &[1, 40, 2, 1, 0, 2, 0, 0, 0, 0, 0, 0]));
            p.extend(frame(b"PrgI", &[0, 0, 0, 1]));
            p.extend(frame(b"PrvI", &[0, 0, 0, 2, 0, 0, 0, 0]));
            p.extend(frame(b"TrPs", &[0, 0, 0, 0, 0, 0, 0, 0]));
            p.extend(frame(b"MPCE", &[0, 1, 0, 0]));
            if fairlight {
                p.extend(fasp(MIC, -1250, 3, 2));
            }
            self.push(&p);
            self.push(&frame(b"InCm", &[1, 0, 0, 0]));
        }

        /// The next datagram the client sends that carries a payload (skips acks and pings).
        async fn next_command(&mut self) -> (u16, Vec<u8>) {
            loop {
                let dg = self.io.from_client.recv().await.unwrap();
                let h = Header::parse(&dg).unwrap();
                if h.flags & flag::ACK_REQUEST != 0 && dg.len() > HEADER_LEN {
                    return (h.packet_id, dg[HEADER_LEN..].to_vec());
                }
            }
        }

        fn ack(&self, id: u16) {
            self.io.to_client.send(atem_ack(SESSION, id)).unwrap();
        }
    }

    fn fasp(src: AudioSourceId, hundredths: i32, supported: u8, mix: u8) -> Vec<u8> {
        let mut body = [0u8; 52];
        body[0..2].copy_from_slice(&src.input.to_be_bytes());
        body[8..16].copy_from_slice(&src.source.to_be_bytes());
        body[44..48].copy_from_slice(&hundredths.to_be_bytes());
        body[48] = supported;
        body[49] = mix;
        frame(b"FASP", &body)
    }

    async fn initialised(fairlight: bool) -> (Atem, Arc<AtemClient>) {
        let (mut atem, client) = Atem::start().await;
        atem.dump(fairlight);
        client.wait_initialised(Duration::from_secs(1)).await.unwrap();
        (atem, client)
    }

    /// The client's next command (acked), as (name, body).
    async fn take(atem: &mut Atem) -> (String, Vec<u8>) {
        let (id, payload) = atem.next_command().await;
        atem.ack(id);
        (String::from_utf8_lossy(&payload[4..8]).into_owned(), payload[8..].to_vec())
    }

    /// A 720p picture of one colour, and its RLE as the switcher would hold it.
    fn flat() -> (Still, Vec<u8>) {
        let rgba = [20u8, 120, 220, 255].repeat(1280 * 720);
        let rle = still::encode_rle(&still::rgba_to_yuv422(1280, 720, &rgba));
        (Still { width: 1280, height: 720, rgba }, rle)
    }

    #[tokio::test(start_paused = true)]
    async fn a_still_downloads_under_the_media_pool_lock() {
        let (mut atem, client) = initialised(false).await;
        atem.push(&frame(b"VidM", &[4, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(client.still_size(), Some((1280, 720)));
        let c = client.clone();
        let call = tokio::spawn(async move { c.download_still(2).await });
        assert_eq!(take(&mut atem).await, ("LOCK".into(), vec![0, 0, 1, 0]));
        atem.push(&frame(b"LKOB", &[0, 0, 0, 0]));
        let (name, body) = take(&mut atem).await;
        assert_eq!(name, "FTSU");
        let id = [body[0], body[1]];
        assert_eq!(body[6..10], [0, 2, 0, 0xf9]);
        let (_, rle) = flat();
        let (a, b) = rle.split_at(16);
        for part in [a, b] {
            let mut d = id.to_vec();
            d.extend_from_slice(&(part.len() as u16).to_be_bytes());
            d.extend_from_slice(part);
            atem.push(&frame(b"FTDa", &d));
            assert_eq!(take(&mut atem).await, ("FTUA".into(), vec![id[0], id[1], 2, 0]));
        }
        atem.push(&frame(b"FTDC", &[id[0], id[1], 0, 0]));
        assert_eq!(take(&mut atem).await, ("LOCK".into(), vec![0, 0, 0, 0]), "unlocked after");
        let got = call.await.unwrap().unwrap();
        assert_eq!((got.width, got.height, got.rgba.len()), (1280, 720, 1280 * 720 * 4));
        for (want, was) in [20u8, 120, 220, 255].iter().zip(&got.rgba[..4]) {
            assert!(want.abs_diff(*was) <= 2, "{:?}", &got.rgba[..4]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_still_uploads_with_its_name_when_the_switcher_asks_for_chunks() {
        let (mut atem, client) = initialised(false).await;
        atem.push(&frame(b"VidM", &[4, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let (picture, rle) = flat();
        let c = client.clone();
        let call = tokio::spawn(async move { c.upload_still(3, picture, "Notices").await });
        assert_eq!(take(&mut atem).await.0, "LOCK");
        atem.push(&frame(b"LKOB", &[0, 0, 0, 0]));
        let (name, body) = take(&mut atem).await;
        assert_eq!(name, "FTSD");
        let id = [body[0], body[1]];
        assert_eq!(body[6..14], [0, 3, 0, 0x38, 0x40, 0, 0, 1], "slot 3, 1280x720x4 bytes, mode 1");
        let mut more = id.to_vec();
        more.extend_from_slice(&[0, 0, 0, 0, 3, 0xe8, 0, 10, 0, 0]);
        atem.push(&frame(b"FTCD", &more));
        let (name, body) = take(&mut atem).await;
        assert_eq!(name, "FTFD");
        assert_eq!(&body[2..9], b"Notices");
        let (name, body) = take(&mut atem).await;
        assert_eq!(name, "FTDa");
        assert_eq!(body[4..], rle[..], "the whole RLE fits one chunk");
        atem.push(&frame(b"FTDC", &[id[0], id[1], 0, 0]));
        assert_eq!(take(&mut atem).await, ("LOCK".into(), vec![0, 0, 0, 0]));
        assert_eq!(call.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_still_cannot_be_moved_while_another_client_holds_the_lock() {
        let (mut atem, client) = initialised(false).await;
        atem.push(&frame(b"VidM", &[4, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.download_still(0).await });
        assert_eq!(take(&mut atem).await.0, "LOCK");
        // No LKOB (the switcher keeps talking, so the link stays up): given up after the wait,
        // and the lock request withdrawn.
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            atem.push(&frame(b"TrPs", &[0, 0, 0, 0, 0, 0, 0, 0]));
        }
        assert_eq!(take(&mut atem).await, ("LOCK".into(), vec![0, 0, 0, 0]));
        let r = call.await.unwrap();
        assert!(matches!(&r, Err(DeviceError::Other(m)) if m.contains("lock")), "{r:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn clearing_a_slot_sends_cstl() {
        let (mut atem, client) = initialised(false).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.clear_still(4).await });
        assert_eq!(take(&mut atem).await, ("CSTL".into(), vec![4, 0, 0, 0]));
        assert_eq!(call.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn program_is_disconnected_until_incm() {
        let (mut atem, client) = Atem::start().await;
        assert!(matches!(client.program().await, Err(DeviceError::Disconnected(_))));
        atem.push(&frame(b"PrgI", &[0, 0, 0, 3]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(matches!(client.program().await, Err(DeviceError::Disconnected(_))));
        assert!(matches!(client.set_preview(2).await, Err(DeviceError::Disconnected(_))));
        atem.push(&frame(b"InCm", &[1, 0, 0, 0]));
        client.wait_initialised(Duration::from_secs(1)).await.unwrap();
        assert_eq!(client.program().await, Ok(3));
    }

    #[tokio::test(start_paused = true)]
    async fn program_and_preview_follow_the_switcher() {
        let (mut atem, client) = initialised(false).await;
        assert_eq!(client.program().await, Ok(1));
        assert_eq!(client.preview().await, Ok(2));
        atem.push(&frame(b"PrgI", &[0, 0, 0x0b, 0xc2]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(client.program().await, Ok(3010));
    }

    #[tokio::test(start_paused = true)]
    async fn set_preview_sends_cpvi_and_resolves_on_ack() {
        let (mut atem, client) = initialised(false).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.set_preview(3).await });
        let (id, payload) = atem.next_command().await;
        assert_eq!(payload, cpvi(0, 3));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!call.is_finished(), "must wait for the ack");
        atem.ack(id);
        assert_eq!(call.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn set_program_and_auto_send_cpgi_and_daut() {
        let (mut atem, client) = initialised(false).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.set_program(8).await });
        let (id, payload) = atem.next_command().await;
        assert_eq!(payload, cpgi(0, 8));
        atem.ack(id);
        assert_eq!(call.await.unwrap(), Ok(()));

        let c = client.clone();
        let call = tokio::spawn(async move { c.auto().await });
        let (id, payload) = atem.next_command().await;
        assert_eq!(payload, daut(0));
        atem.ack(id);
        assert_eq!(call.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn unacked_command_times_out() {
        let (mut atem, client) = initialised(false).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.set_preview(3).await });
        let _ = atem.next_command().await;
        let r = call.await.unwrap();
        assert!(matches!(r, Err(DeviceError::Timeout(_))), "{r:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_transition_done_immediately_when_idle() {
        let (_atem, client) = initialised(false).await;
        let t0 = Instant::now();
        assert_eq!(client.wait_transition_done(Duration::from_secs(5)).await, Ok(()));
        assert_eq!(Instant::now(), t0);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_transition_done_resolves_when_trps_goes_idle() {
        let (mut atem, client) = initialised(false).await;
        atem.push(&frame(b"TrPs", &[0, 1, 30, 0, 0, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let c = client.clone();
        let wait = tokio::spawn(async move { c.wait_transition_done(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!wait.is_finished());
        atem.push(&frame(b"TrPs", &[0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(wait.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_transition_done_times_out() {
        let (mut atem, client) = initialised(false).await;
        atem.push(&frame(b"TrPs", &[0, 1, 30, 0, 0, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let r = client.wait_transition_done(Duration::from_millis(300)).await;
        assert!(matches!(r, Err(DeviceError::Timeout(_))), "{r:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_after_auto_waits_for_the_transition_to_start() {
        // The DAut ack can arrive before the TrPs that starts the transition: the wait must not
        // report done on the stale idle state.
        let (mut atem, client) = initialised(false).await;
        let c = client.clone();
        let call = tokio::spawn(async move { c.auto().await });
        let (id, _) = atem.next_command().await;
        atem.ack(id);
        call.await.unwrap().unwrap();
        let c = client.clone();
        let wait = tokio::spawn(async move { c.wait_transition_done(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!wait.is_finished());
        atem.push(&frame(b"TrPs", &[0, 1, 30, 0, 0, 0, 0, 0]));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!wait.is_finished());
        atem.push(&frame(b"TrPs", &[0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(wait.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn media_player_still_sends_mpss_for_the_wire_player() {
        let (mut atem, client) = initialised(false).await;
        assert_eq!(client.media_player_input(1), 3010);
        assert_eq!(client.media_player_input(2), 3020);
        let c = client.clone();
        let call = tokio::spawn(async move { c.set_media_player_still(2, 7).await });
        let (id, payload) = atem.next_command().await;
        assert_eq!(payload, Mpss::still(1, 7).encode());
        atem.ack(id);
        assert_eq!(call.await.unwrap(), Ok(()));
        // Topology says two players; still indexes are one byte.
        assert!(matches!(client.set_media_player_still(0, 1).await, Err(DeviceError::Refused(_))));
        assert!(matches!(client.set_media_player_still(3, 1).await, Err(DeviceError::Refused(_))));
        assert!(matches!(client.set_media_player_still(1, 256).await, Err(DeviceError::Refused(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn audio_level_reads_fasp_in_db() {
        let (mut atem, client) = initialised(true).await;
        assert_eq!(client.audio_level(MIC).await, Ok(-12.5));
        atem.push(&fasp(MIC, -10000, 3, 2));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(client.audio_level(MIC).await, Ok(-100.0));
        let other = AudioSourceId { input: 1002, source: -65280 };
        assert!(matches!(client.audio_level(other).await, Err(DeviceError::Refused(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn set_audio_level_sends_cfsp_with_only_the_fader_bit() {
        let (mut atem, client) = initialised(true).await;
        for (db, wire) in [(-10.0f32, -1000), (f32::NEG_INFINITY, -10000), (-250.0, -10000), (3.456, 346)] {
            let c = client.clone();
            let call = tokio::spawn(async move { c.set_audio_level(MIC, db).await });
            let (id, payload) = atem.next_command().await;
            assert_eq!(payload, Cfsp::fader_gain(1001, -65280, wire).encode(), "{db}");
            atem.ack(id);
            assert_eq!(call.await.unwrap(), Ok(()));
        }
        assert!(matches!(client.set_audio_level(MIC, 10.5).await, Err(DeviceError::Refused(_))));
        assert!(matches!(client.set_audio_level(MIC, f32::NAN).await, Err(DeviceError::Refused(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn set_audio_on_sends_cfsp_with_only_the_mix_option_bit() {
        let (mut atem, client) = initialised(true).await;
        for (on, opt) in [(true, mix_option::ON), (false, mix_option::OFF)] {
            let c = client.clone();
            let call = tokio::spawn(async move { c.set_audio_on(MIC, on).await });
            let (id, payload) = atem.next_command().await;
            assert_eq!(payload, Cfsp::mix_option(1001, -65280, opt).encode());
            atem.ack(id);
            assert_eq!(call.await.unwrap(), Ok(()));
        }
        // A source that only supports Audio Follow Video cannot be switched On.
        let afv = AudioSourceId { input: 1, source: -65280 };
        atem.push(&fasp(afv, 0, 4, 4));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(matches!(client.set_audio_on(afv, true).await, Err(DeviceError::Refused(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn fairlight_calls_are_refused_without_fairlight() {
        let (_atem, client) = initialised(false).await;
        for r in [
            client.audio_level(MIC).await.map(|_| ()),
            client.set_audio_level(MIC, -10.0).await,
            client.set_audio_on(MIC, true).await,
        ] {
            assert_eq!(r, Err(DeviceError::Refused("no fairlight".into())));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_link_forgets_the_state() {
        let (_atem, client) = initialised(false).await;
        assert_eq!(client.program().await, Ok(1));
        // Nothing more from the switcher: after 3 s the link is lost.
        tokio::time::sleep(Duration::from_millis(3100)).await;
        assert!(matches!(client.program().await, Err(DeviceError::Disconnected(_))));
        assert!(!client.snapshot().connected);
    }
}
