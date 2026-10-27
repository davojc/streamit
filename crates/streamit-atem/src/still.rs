//! Media pool stills over the ATEM data-transfer protocol: the pixel format, its RLE, and
//! the lock, download and upload exchanges.
//!
//! Ported from atem-connection (MIT, github.com/nrkno/sofie-atem-connection, `main`):
//! src/lib/converters/{rle,rgbaToYuv422,yuv422ToRgba,colorConstants}.ts,
//! src/lib/videoMode.ts and src/dataTransfer/{dataTransferQueue,dataTransferDownloadBuffer,
//! dataTransferUploadBuffer,dataTransferUploadStill}.ts.
//!
//! A still is 10-bit YUV 4:2:2 with alpha, 4 bytes a pixel: each pixel is a u32 BE of
//! alpha << 20 | chroma << 10 | luma, the first of a pair carrying Cb and the second Cr.
//! BT.709 from 720 lines up, BT.601 below.
use crate::AtemClient;
use crate::commands::{self, Decoded};
use std::sync::atomic::Ordering;
use std::time::Duration;
use streamit_core::devices::{DeviceError, Result};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};

/// Marks a run in the RLE: the header, a u64 count, then the 8-byte block repeated.
pub const RLE_HEADER: u64 = 0xFEFE_FEFE_FEFE_FEFE;
/// The media pool's stills are store 0.
const STILLS: u16 = 0;
/// How long to wait for the lock, and for each reply during a transfer.
const LOCK_WAIT: Duration = Duration::from_secs(5);
const REPLY_WAIT: Duration = Duration::from_secs(5);
/// Retries when the switcher asks for one (upstream `ErrorCode.Retry`).
const RETRIES: u32 = 3;

/// A still's width and height for a video mode (upstream `getVideoModeInfo`).
pub fn mode_size(mode: u8) -> Option<(u32, u32)> {
    Some(match mode {
        0 | 2 => (640, 480),
        1 | 3 => (720, 576),
        4 | 5 => (1280, 720),
        6..=13 | 26 | 27 => (1920, 1080),
        14..=19 => (3840, 2160),
        20..=25 => (7680, 4320),
        _ => return None,
    })
}

fn block(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Expands RLE data to `full` bytes (shorter input leaves the rest zero; longer is cut).
pub fn decode_rle(data: &[u8], full: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(full);
    let mut at = 0;
    while let Some(b) = block(data, at) {
        if out.len() >= full {
            break;
        }
        if b == RLE_HEADER {
            let (Some(count), Some(rep)) = (block(data, at + 8), block(data, at + 16)) else { break };
            let room = ((full - out.len()) / 8) as u64;
            for _ in 0..count.min(room) {
                out.extend_from_slice(&rep.to_be_bytes());
            }
            at += 24;
        } else {
            out.extend_from_slice(&b.to_be_bytes());
            at += 8;
        }
    }
    out.resize(full, 0);
    out
}

/// Compresses runs of four or more identical 8-byte blocks (upstream `encodeRLE`).
pub fn encode_rle(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let blocks: Vec<u64> = (0..data.len() / 8).filter_map(|i| block(data, i * 8)).collect();
    let mut i = 0;
    while i < blocks.len() {
        let run = blocks[i..].iter().take_while(|&&b| b == blocks[i]).count();
        if run >= 4 {
            out.extend_from_slice(&RLE_HEADER.to_be_bytes());
            out.extend_from_slice(&(run as u64).to_be_bytes());
            out.extend_from_slice(&blocks[i].to_be_bytes());
        } else {
            for _ in 0..run {
                out.extend_from_slice(&blocks[i].to_be_bytes());
            }
        }
        i += run;
    }
    out
}

/// Upstream `createColorConvertConstants`.
struct Colour {
    kr: f64,
    kb: f64,
    kg: f64,
    kri: f64,
    kbi: f64,
}

const Y_RANGE: f64 = 219.0;
const HALF_CBCR: f64 = 112.0;
const Y_OFFSET: f64 = (16 << 8) as f64;
const CBCR_OFFSET: f64 = (128 << 8) as f64;

fn colour(height: u32) -> Colour {
    let (kr, kb) = if height >= 720 { (0.2126, 0.0722) } else { (0.299, 0.114) };
    Colour { kr, kb, kg: 1.0 - kr - kb, kri: 1.0 - kr, kbi: 1.0 - kb }
}

/// Upstream `convertRGBAToYUV422`.
pub fn rgba_to_yuv422(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let c = colour(height);
    let len = (width * height * 4) as usize;
    let mut out = vec![0u8; len];
    let sample = |a: u8, uv16: f64, y16: f64| -> u32 {
        let a = ((u32::from(a) << 2) * 219) / 255 + (16 << 2);
        let y = (y16.round() as i64 >> 6).clamp(0, 0x3ff) as u32;
        let uv = (uv16.round() as i64 >> 6).clamp(0, 0x3ff) as u32;
        (a << 20) + (uv << 10) + y
    };
    let px = |i: usize| -> [f64; 4] {
        let p = rgba.get(i..i + 4).unwrap_or(&[0, 0, 0, 255]);
        [f64::from(p[0]), f64::from(p[1]), f64::from(p[2]), f64::from(p[3])]
    };
    for i in (0..len).step_by(8) {
        let [r1, g1, b1, a1] = px(i);
        let [r2, g2, b2, a2] = px(i + 4);
        let y1 = Y_OFFSET + c.kr * Y_RANGE * r1 + c.kg * Y_RANGE * g1 + c.kb * Y_RANGE * b1;
        let y2 = Y_OFFSET + c.kr * Y_RANGE * r2 + c.kg * Y_RANGE * g2 + c.kb * Y_RANGE * b2;
        let cb = CBCR_OFFSET + (-(c.kr / c.kbi * HALF_CBCR) * r1 - (c.kg / c.kbi * HALF_CBCR) * g1 + HALF_CBCR * b1);
        let cr = CBCR_OFFSET + (HALF_CBCR * r1 - (c.kg / c.kri * HALF_CBCR) * g1 - (c.kb / c.kri * HALF_CBCR) * b1);
        out[i..i + 4].copy_from_slice(&sample(a1 as u8, cb, y1).to_be_bytes());
        out[i + 4..i + 8].copy_from_slice(&sample(a2 as u8, cr, y2).to_be_bytes());
    }
    out
}

/// Upstream `convertYUV422ToRGBA`, except green: upstream doubles both chroma terms there, which
/// takes green far too low (a mid red-orange comes back with no green). With chroma scaled as
/// `rgba_to_yuv422` writes it, G = Y - Kr(1-Kr)/Kg Cr - Kb(1-Kb)/Kg Cb.
pub fn yuv422_to_rgba(width: u32, height: u32, yuv: &[u8]) -> Vec<u8> {
    let c = colour(height);
    let len = (width * height * 4) as usize;
    let mut out = vec![0u8; len];
    let split = |raw: u32| -> (f64, f64, f64) {
        let y = f64::from((raw & 0x3ff) << 6);
        let uv = f64::from((raw & 0xffc00) >> 4);
        let a = f64::from((raw & 0x3ff0_0000) >> 20);
        ((y - Y_OFFSET) / Y_RANGE, (uv - CBCR_OFFSET) / HALF_CBCR, ((a - 64.0) / 219.0) * 255.0 / 4.0)
    };
    let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    for i in (0..len).step_by(8) {
        let (Some(s1), Some(s2)) = (block(yuv, i).map(|b| (b >> 32) as u32), block(yuv, i).map(|b| b as u32)) else {
            break;
        };
        let (y1, cb, a1) = split(s1);
        let (y2, cr, a2) = split(s2);
        let rgb = |y: f64| {
            [
                byte(y + c.kri * cr),
                byte(y - (c.kr * c.kri / c.kg) * cr - (c.kb * c.kbi / c.kg) * cb),
                byte(y + c.kbi * cb),
            ]
        };
        let ([r1, g1, b1], [r2, g2, b2]) = (rgb(y1), rgb(y2));
        out[i..i + 8].copy_from_slice(&[r1, g1, b1, byte(a1), r2, g2, b2, byte(a2)]);
    }
    out
}

/// The chunks for one `FTCD`: `count` of at most `size` bytes (a multiple of 8) from `at`,
/// never ending inside an RLE run (upstream `getNextChunks`). Returns the chunks' ends.
pub fn next_chunks(data: &[u8], mut at: usize, size: u16, count: u16) -> Vec<(usize, usize)> {
    let size = usize::from(size) / 8 * 8;
    let mut chunks = Vec::new();
    if size == 0 {
        return chunks;
    }
    for _ in 0..count {
        if at >= data.len() {
            break;
        }
        let mut end = (at + size).min(data.len());
        if end < data.len() {
            if block(data, end - 8) == Some(RLE_HEADER) {
                end -= 8;
            } else if end >= at + 16 && block(data, end - 16) == Some(RLE_HEADER) {
                end -= 16;
            }
        }
        chunks.push((at, end));
        at = end;
    }
    chunks
}

/// What one attempt at a transfer ended with.
enum Attempt<R> {
    Done(R),
    /// The switcher asked to start again (upstream `ErrorCode.Retry`).
    Retry,
}

/// Waits for the next transfer reply for `id` (lock replies have no id and always pass).
async fn reply(rx: &mut mpsc::UnboundedReceiver<Decoded>, id: u16) -> Result<Decoded> {
    loop {
        let d = timeout(REPLY_WAIT, rx.recv())
            .await
            .map_err(|_| DeviceError::Timeout("the switcher stopped answering the transfer".into()))?
            .ok_or_else(|| DeviceError::Other("the switcher connection closed".into()))?;
        let for_us = match &d {
            Decoded::TransferData { id: i, .. }
            | Decoded::TransferComplete(i)
            | Decoded::TransferError { id: i, .. }
            | Decoded::TransferContinue { id: i, .. } => *i == id,
            _ => true,
        };
        if for_us {
            return Ok(d);
        }
    }
}

fn transfer_error(code: u8) -> DeviceError {
    DeviceError::Other(match code {
        2 => "the switcher has no such still".into(),
        5 => "the media pool was not locked".into(),
        c => format!("the switcher refused the transfer (error {c})"),
    })
}

/// The largest still any switcher sends (8K, 4 bytes a pixel, with 10% for the RLE).
const MAX_STILL: usize = 7680 * 4320 * 4 * 11 / 10;

impl AtemClient {
    pub(crate) fn size_for_stills(&self) -> Result<(u32, u32)> {
        self.read(|s| {
            s.video_mode
                .and_then(mode_size)
                .ok_or_else(|| DeviceError::Other("the switcher has not said its video format".into()))
        })
    }

    /// Takes our turn (one transfer at a time), routes transfer replies to us and locks the
    /// stills. Always follow with `end_transfer`.
    async fn begin_transfer(&self) -> Result<(tokio::sync::MutexGuard<'_, ()>, mpsc::UnboundedReceiver<Decoded>)> {
        let turn = self.transfer_turn.lock().await;
        self.read(|_| Ok(()))?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        *self.transfer.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let locked = async {
            self.send(commands::lock(STILLS, true)).await?;
            timeout(LOCK_WAIT, async {
                while let Some(d) = rx.recv().await {
                    if d == Decoded::LockObtained(STILLS) {
                        return Ok(());
                    }
                }
                Err(DeviceError::Other("the switcher connection closed".into()))
            })
            .await
            .map_err(|_| DeviceError::Other("could not lock the media pool: is another client using it?".into()))?
        };
        if let Err(e) = locked.await {
            self.end_transfer().await;
            return Err(e);
        }
        Ok((turn, rx))
    }

    async fn end_transfer(&self) {
        *self.transfer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Err(e) = self.send(commands::lock(STILLS, false)).await {
            warn!("could not unlock the media pool: {e}");
        }
    }

    fn transfer_id(&self) -> u16 {
        self.next_transfer.fetch_add(1, Ordering::Relaxed) & 0x7fff
    }

    /// The still in slot `index` as the switcher stores it (RLE, YUV).
    pub(crate) async fn download(&self, index: u8) -> Result<Vec<u8>> {
        let (_turn, mut rx) = self.begin_transfer().await?;
        let mut result = Err(DeviceError::Other("the switcher kept asking to retry".into()));
        for _ in 0..=RETRIES {
            match self.download_once(index, &mut rx).await {
                Ok(Attempt::Done(data)) => {
                    result = Ok(data);
                    break;
                }
                Ok(Attempt::Retry) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.end_transfer().await;
        result
    }

    async fn download_once(&self, index: u8, rx: &mut mpsc::UnboundedReceiver<Decoded>) -> Result<Attempt<Vec<u8>>> {
        let id = self.transfer_id();
        info!(id, slot = index, "ATEM: downloading a still");
        self.send(commands::ftsu(id, STILLS, u16::from(index))).await?;
        let mut data = Vec::new();
        loop {
            match reply(rx, id).await? {
                Decoded::TransferData { body, .. } => {
                    data.extend_from_slice(&body);
                    if data.len() > MAX_STILL {
                        return Err(DeviceError::Other("the still is larger than any switcher's".into()));
                    }
                    self.send(commands::ftua(id, index)).await?;
                }
                Decoded::TransferComplete(_) => return Ok(Attempt::Done(data)),
                Decoded::TransferError { code: 1, .. } => return Ok(Attempt::Retry),
                Decoded::TransferError { code, .. } => return Err(transfer_error(code)),
                Decoded::LockState { locked: false, index: STILLS } => {
                    return Err(DeviceError::Other("lost the media pool lock".into()));
                }
                _ => {}
            }
        }
    }

    /// Sends `data` (the RLE of `size` bytes, hashed as `hash`) to slot `slot`, named `name`.
    pub(crate) async fn upload(&self, slot: u16, size: u32, data: &[u8], name: &str, hash: &[u8; 16]) -> Result<()> {
        let (_turn, mut rx) = self.begin_transfer().await?;
        let mut result = Err(DeviceError::Other("the switcher kept asking to retry".into()));
        for _ in 0..=RETRIES {
            match self.upload_once(slot, size, data, name, hash, &mut rx).await {
                Ok(Attempt::Done(())) => {
                    result = Ok(());
                    break;
                }
                Ok(Attempt::Retry) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.end_transfer().await;
        result
    }

    async fn upload_once(
        &self,
        slot: u16,
        size: u32,
        data: &[u8],
        name: &str,
        hash: &[u8; 16],
        rx: &mut mpsc::UnboundedReceiver<Decoded>,
    ) -> Result<Attempt<()>> {
        let id = self.transfer_id();
        info!(id, slot, bytes = data.len(), "ATEM: uploading a still");
        self.send(commands::ftsd(id, STILLS, slot, size)).await?;
        let mut sent = 0;
        let mut described = false;
        loop {
            match reply(rx, id).await? {
                Decoded::TransferContinue { chunk_size, chunk_count, .. } => {
                    if !described {
                        described = true;
                        self.send(commands::ftfd(id, name, "", hash)).await?;
                    }
                    for (from, to) in next_chunks(data, sent, chunk_size, chunk_count) {
                        self.send(commands::ftda(id, &data[from..to])).await?;
                        sent = to;
                    }
                }
                Decoded::TransferComplete(_) if described => return Ok(Attempt::Done(())),
                Decoded::TransferError { code: 1, .. } => return Ok(Attempt::Retry),
                Decoded::TransferError { code, .. } => return Err(transfer_error(code)),
                Decoded::LockState { locked: false, index: STILLS } => {
                    return Err(DeviceError::Other("lost the media pool lock".into()));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                // A flat band (so the RLE has runs), then a gradient.
                let c =
                    if y < h / 2 { [200, 30, 40, 255] } else { [(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255] };
                v.extend_from_slice(&c);
            }
        }
        v
    }

    #[test]
    fn video_modes_give_still_sizes() {
        assert_eq!(mode_size(11), Some((1920, 1080)));
        assert_eq!(mode_size(4), Some((1280, 720)));
        assert_eq!(mode_size(17), Some((3840, 2160)));
        assert_eq!(mode_size(99), None);
    }

    #[test]
    fn rle_packs_runs_of_four_or_more_and_round_trips() {
        let raw: Vec<u8> =
            [[0xAAu8; 8], [0xAA; 8], [0xAA; 8], [0xAA; 8], [0xAA; 8], [0xBB; 8], [0xCC; 8], [0xCC; 8]].concat();
        let mut want = RLE_HEADER.to_be_bytes().to_vec();
        want.extend_from_slice(&5u64.to_be_bytes());
        want.extend_from_slice(&[0xAA; 8]);
        want.extend_from_slice(&[0xBB; 8]);
        want.extend_from_slice(&[0xCC; 16]);
        assert_eq!(encode_rle(&raw), want, "five AAs as a run; BB and the two CCs as they are");
        assert_eq!(decode_rle(&want, raw.len()), raw);
        let still = rgba_to_yuv422(64, 720, &picture(64, 720));
        assert_eq!(decode_rle(&encode_rle(&still), still.len()), still);
        assert!(encode_rle(&still).len() < still.len(), "the flat band packs");
    }

    #[test]
    fn colours_survive_a_round_trip_through_yuv() {
        let (w, h) = (64, 720);
        let rgba = picture(w, h);
        let back = yuv422_to_rgba(w, h, &rgba_to_yuv422(w, h, &rgba));
        // 4:2:2 shares chroma between a pixel pair; a flat colour comes back within a step.
        let at = |v: &[u8], i: usize| v[i * 4..i * 4 + 4].to_vec();
        for (a, b) in at(&rgba, 10).iter().zip(at(&back, 10)) {
            assert!(a.abs_diff(b) <= 2, "{:?} vs {:?}", at(&rgba, 10), at(&back, 10));
        }
    }

    #[test]
    fn chunks_never_end_inside_a_run() {
        // 16 plain bytes, a run at 16..40, then plain.
        let mut data = vec![1u8; 16];
        data.extend_from_slice(&RLE_HEADER.to_be_bytes());
        data.extend_from_slice(&3u64.to_be_bytes());
        data.extend_from_slice(&[5; 8]);
        data.extend_from_slice(&[6; 16]);
        // A 24-byte chunk would end after the run's header: it stops before it instead.
        assert_eq!(next_chunks(&data, 0, 24, 1), vec![(0, 16)]);
        // A 32-byte chunk would end after the count: it stops before the header too.
        assert_eq!(next_chunks(&data, 0, 32, 1), vec![(0, 16)]);
        assert_eq!(next_chunks(&data, 16, 1000, 5), vec![(16, data.len())], "the last chunk takes the rest");
        assert_eq!(next_chunks(&data, data.len(), 24, 3), vec![], "nothing left");
    }
}
