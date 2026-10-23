//! Finds Blackmagic devices on the network by mDNS (`_blackmagic._tcp.local.`).
//!
//! TXT source: the Bitfocus Companion module manifests filter this service type on the TXT
//! key `class`: companion-module-bmd-atem `companion/manifest.json` uses
//! `{"class": "AtemSwitcher"}` and companion-module-bmd-videohub uses `{"class": "Videohub"}`.
//! The Web Presenter module has no bonjour query, so `WebPresenter` / `Streaming` are
//! unconfirmed (TODO(capture): check a real Web Presenter 4K's TXT record). The model is
//! read from TXT `model`, else `name` (also unconfirmed); the instance name is the
//! service's own name.
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, TxtProperties};
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const SERVICE_TYPE: &str = "_blackmagic._tcp.local.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmdKind {
    Atem,
    VideoHub,
    WebPresenter,
}

impl BmdKind {
    pub fn label(self) -> &'static str {
        match self {
            BmdKind::Atem => "ATEM",
            BmdKind::VideoHub => "VideoHub",
            BmdKind::WebPresenter => "Web Presenter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub name: String,
    pub model: String,
    pub address: IpAddr,
    pub kind: Option<BmdKind>,
}

/// The device kind from the TXT `class` value; anything else is `None`.
pub fn kind_from_txt(txt: &TxtProperties) -> Option<BmdKind> {
    match txt.get_property_val_str("class")?.trim() {
        "AtemSwitcher" => Some(BmdKind::Atem),
        "Videohub" => Some(BmdKind::VideoHub),
        "WebPresenter" | "Streaming" => Some(BmdKind::WebPresenter),
        _ => None,
    }
}

/// Browses for `timeout` and returns each device found once (first IPv4 address).
/// Blocking: run it off the UI thread and off async worker threads.
pub fn browse(timeout: Duration) -> Vec<DiscoveredDevice> {
    let mut found: Vec<DiscoveredDevice> = Vec::new();
    browse_with(timeout, |s| {
        let Some(ip) = s.get_addresses_v4().into_iter().min() else { return };
        let txt = s.get_properties();
        let dev = DiscoveredDevice {
            name: instance_name(s.get_fullname()),
            model: txt
                .get_property_val_str("model")
                .or_else(|| txt.get_property_val_str("name"))
                .unwrap_or("")
                .to_string(),
            address: IpAddr::V4(ip),
            kind: kind_from_txt(txt),
        };
        if !found.iter().any(|d| d.address == dev.address) {
            info!(name = %dev.name, model = %dev.model, address = %dev.address, kind = ?dev.kind, "discovered");
            found.push(dev);
        }
    });
    found.sort_by_key(|d| d.address);
    found
}

/// Calls `on_resolved` for every `_blackmagic._tcp` service resolved within `timeout`.
/// Blocking.
pub fn browse_with(timeout: Duration, mut on_resolved: impl FnMut(&ResolvedService)) {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("mDNS discovery could not start: {e}");
            return;
        }
    };
    match daemon.browse(SERVICE_TYPE) {
        Ok(rx) => {
            let deadline = Instant::now() + timeout;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(ServiceEvent::ServiceResolved(s)) => on_resolved(&s),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
        Err(e) => warn!("mDNS browse failed: {e}"),
    }
    let _ = daemon.shutdown();
}

/// "Stream ATEM._blackmagic._tcp.local." -> "Stream ATEM".
fn instance_name(fullname: &str) -> String {
    fullname.strip_suffix(SERVICE_TYPE).unwrap_or(fullname).trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::IntoTxtProperties;

    fn txt(pairs: &[(&str, &str)]) -> TxtProperties {
        pairs.into_txt_properties()
    }

    #[test]
    fn kind_from_txt_maps_classes() {
        assert_eq!(kind_from_txt(&txt(&[("class", "AtemSwitcher")])), Some(BmdKind::Atem));
        assert_eq!(kind_from_txt(&txt(&[("class", "Videohub")])), Some(BmdKind::VideoHub));
        assert_eq!(kind_from_txt(&txt(&[("class", "WebPresenter")])), Some(BmdKind::WebPresenter));
        assert_eq!(kind_from_txt(&txt(&[("class", "Streaming")])), Some(BmdKind::WebPresenter));
        // TXT keys are case-insensitive (RFC 6763), other keys are ignored.
        assert_eq!(
            kind_from_txt(&txt(&[("name", "ATEM Television Studio HD8 ISO"), ("Class", "AtemSwitcher")])),
            Some(BmdKind::Atem)
        );
    }

    #[test]
    fn unknown_class_is_none() {
        assert_eq!(kind_from_txt(&txt(&[("class", "HyperDeck")])), None);
        assert_eq!(kind_from_txt(&txt(&[("class", "")])), None);
        assert_eq!(kind_from_txt(&txt(&[("name", "Videohub")])), None);
        assert_eq!(kind_from_txt(&txt(&[])), None);
    }

    #[test]
    fn instance_name_strips_the_service_type() {
        assert_eq!(instance_name("Stream ATEM._blackmagic._tcp.local."), "Stream ATEM");
        assert_eq!(instance_name("odd"), "odd");
    }
}
