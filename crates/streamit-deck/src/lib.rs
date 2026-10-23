//! Stream Deck XL surface: key presses become engine `Command`s and every key is
//! redrawn from the engine `Snapshot`.
//!
//! Hardware API source: elgato-streamdeck 0.14.0 (docs.rs/elgato-streamdeck/0.14.0,
//! `AsyncStreamDeck` and examples/async.rs). `set_button_image(key, image)` JPEG-encodes
//! at the model's key size (96x96 for the XL) and applies the XL's mirroring itself;
//! `key` 0..32 runs row-major from the top-left, matching `layout`'s `row * 8 + col`.
//! The async wrapper uses `block_in_place`, so it needs a tokio multi-thread runtime.
pub mod layout;
pub mod probe;
pub mod render;

use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, StreamDeckError, list_devices, new_hidapi};
use layout::{KEYS, KeyLook, appearance, command_for};
use std::time::Duration;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, EngineHandle, Snapshot};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, block_in_place};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

/// How often to look for the deck while it is absent or unplugged.
pub const RECONNECT_EVERY: Duration = Duration::from_secs(2);
/// Blink toggle period: toggling every 250 ms is a 2 Hz blink (two on/off cycles a second).
pub const BLINK_TOGGLE: Duration = Duration::from_millis(250);
const BRIGHTNESS: u8 = 80;

pub fn is_xl(kind: Kind) -> bool {
    matches!(kind, Kind::Xl | Kind::XlV2 | Kind::XlV2Module)
}

/// Opens the first Stream Deck XL on the bus, if one is present.
fn connect_first() -> Option<AsyncStreamDeck> {
    // A fresh HidApi each time so a replugged deck is enumerated.
    let hid = block_in_place(new_hidapi).ok()?;
    let (kind, serial) = list_devices(&hid).into_iter().find(|(k, _)| is_xl(*k))?;
    match AsyncStreamDeck::connect(&hid, kind, &serial) {
        Ok(d) => Some(d),
        Err(e) => {
            warn!("stream deck found but could not be opened (is Companion running?): {e}");
            None
        }
    }
}

struct SessionError(String);
impl From<StreamDeckError> for SessionError {
    fn from(e: StreamDeckError) -> Self {
        Self(e.to_string())
    }
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct DeckSurface;

impl DeckSurface {
    /// Opens the first Stream Deck XL found, reconnects when it goes away and redraws
    /// on every snapshot. Must run inside a tokio multi-thread runtime.
    pub fn run(mut handle: EngineHandle) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut reported: Option<bool> = None;
            loop {
                if handle.tx.is_closed() {
                    return; // the engine is gone, deck or no deck
                }
                let Some(dev) = connect_first() else {
                    report(&handle, &mut reported, false).await;
                    tokio::time::sleep(RECONNECT_EVERY).await;
                    continue;
                };
                info!("stream deck connected");
                report(&handle, &mut reported, true).await;
                match session(&dev, &mut handle).await {
                    Ok(()) => return, // the engine is gone
                    Err(e) => warn!("stream deck session ended: {e}"),
                }
                report(&handle, &mut reported, false).await;
                tokio::time::sleep(RECONNECT_EVERY).await;
            }
        })
    }
}

async fn report(handle: &EngineHandle, last: &mut Option<bool>, up: bool) {
    if *last == Some(up) {
        return;
    }
    *last = Some(up);
    let health = if up { DeviceHealth::Ok } else { DeviceHealth::Down("Stream Deck not connected".into()) };
    let _ = handle.tx.send(Command::DeviceHealth(DeviceKind::StreamDeck, health)).await;
}

/// The command for a press, decided from the latest published snapshot (not a cached copy).
fn decide_press(key: u8, rx: &watch::Receiver<Snapshot>) -> Option<Command> {
    command_for(key, &rx.borrow())
}

/// Redraws the keys whose look changed since `cache`, then flushes.
async fn draw(
    dev: &AsyncStreamDeck,
    snap: &Snapshot,
    phase: bool,
    cache: &mut [Option<KeyLook>],
) -> Result<(), SessionError> {
    let mut changed = false;
    for key in 0..KEYS {
        let look = appearance(key, snap, phase);
        if cache[key as usize].as_ref() == Some(&look) {
            continue;
        }
        dev.set_button_image(key, render::render_key(&look)).await?;
        cache[key as usize] = Some(look);
        changed = true;
    }
    if changed {
        dev.flush().await?;
    }
    Ok(())
}

/// Runs one connection. `Ok` means the engine has gone; `Err` means the deck failed.
async fn session(dev: &AsyncStreamDeck, handle: &mut EngineHandle) -> Result<(), SessionError> {
    dev.set_brightness(BRIGHTNESS).await?;
    dev.clear_all_button_images().await?;

    // Button reads run in their own task: cancelling a read inside select! could drop an event.
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<Result<u8, StreamDeckError>>();
    let reader = dev.get_reader();
    let reader_task = tokio::spawn(async move {
        // The crate's async `read` polls the HID device non-blocking and sleeps 1/60 s between
        // polls; it has no timeout and only returns on an event or an error. So the reads are
        // never cancelled mid-event: the session ends the task with `abort()`, which lands on
        // that sleep, and awaits it so the HID handle is released before a reconnect.
        while !key_tx.is_closed() {
            match reader.read(60.0).await {
                Ok(updates) => {
                    for u in updates {
                        if let DeviceStateUpdate::ButtonDown(k) = u {
                            let _ = key_tx.send(Ok(k));
                        }
                    }
                }
                Err(e) => {
                    let _ = key_tx.send(Err(e));
                    return;
                }
            }
        }
    });

    let result = async {
        let mut cache: Vec<Option<KeyLook>> = vec![None; KEYS as usize];
        let mut phase = true;
        let mut ticker = interval(BLINK_TOGGLE);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut snap = handle.rx.borrow_and_update().clone();
        draw(dev, &snap, phase, &mut cache).await?;
        loop {
            tokio::select! {
                changed = handle.rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    snap = handle.rx.borrow_and_update().clone();
                    draw(dev, &snap, phase, &mut cache).await?;
                }
                _ = ticker.tick() => {
                    phase = !phase;
                    draw(dev, &snap, phase, &mut cache).await?;
                }
                press = key_rx.recv() => match press {
                    Some(Ok(key)) => {
                        if let Some(cmd) = decide_press(key, &handle.rx)
                            && handle.tx.send(cmd).await.is_err()
                        {
                            return Ok(());
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Err(SessionError("key reader stopped".into())),
                },
            }
        }
    }
    .await;
    reader_task.abort();
    let _ = reader_task.await; // wait until the reader (and its HID handle) is dropped
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::recovery::Offer;

    #[test]
    fn press_uses_latest_snapshot() {
        let (tx, rx) = watch::channel(Snapshot { offers: vec![Offer::Recover], ..Snapshot::default() });
        assert_eq!(decide_press(15, &rx), Some(Command::Recover));
        // The offer is withdrawn; the receiver never polled changed().
        tx.send(Snapshot::default()).unwrap();
        assert_eq!(decide_press(15, &rx), None);
    }
}
