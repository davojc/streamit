//! Finds Blackmagic devices on the network by mDNS (`_blackmagic._tcp.local.`).
//!
//! TXT source: the Bitfocus Companion module manifests filter this service type on the TXT
//! key `class`: companion-module-bmd-atem `companion/manifest.json` uses
//! `{"class": "AtemSwitcher"}` and companion-module-bmd-videohub uses `{"class": "Videohub"}`.
//! The Web Presenter module has no bonjour query, so `WebPresenter` / `Streaming` are
//! unconfirmed (TODO(capture): check a real Web Presenter 4K's TXT record). The model is
//! read from TXT `model`, else `name` (also unconfirmed); the instance name is the
//! service's own name. A MultiView 4 speaks the Videohub protocol, so it is told apart
//! from a VideoHub by "MultiView" in its model or name.
//!
//! Some ATEMs (seen with a Television Studio HD8 ISO) do not advertise over mDNS, so
//! `sweep_atems` also says the ATEM hello (UDP 9910) to every address on this computer's
//! own /24 networks and lists whatever answers.
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, TxtProperties};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const SERVICE_TYPE: &str = "_blackmagic._tcp.local.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmdKind {
    Atem,
    VideoHub,
    MultiView,
    WebPresenter,
}

impl BmdKind {
    pub fn label(self) -> &'static str {
        match self {
            BmdKind::Atem => "ATEM",
            BmdKind::VideoHub => "VideoHub",
            BmdKind::MultiView => "MultiView",
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

/// The device kind from the TXT `class` value (and, for the Videohub protocol, the
/// model); anything else is `None`.
pub fn kind_from_txt(txt: &TxtProperties) -> Option<BmdKind> {
    let multiview = ["model", "name"]
        .iter()
        .any(|k| txt.get_property_val_str(k).is_some_and(|v| v.to_ascii_lowercase().contains("multiview")));
    match txt.get_property_val_str("class")?.trim() {
        "AtemSwitcher" => Some(BmdKind::Atem),
        "Videohub" if multiview => Some(BmdKind::MultiView),
        "Videohub" => Some(BmdKind::VideoHub),
        c if c.eq_ignore_ascii_case("multiview") => Some(BmdKind::MultiView),
        "WebPresenter" | "Streaming" => Some(BmdKind::WebPresenter),
        _ => None,
    }
}

/// Every device found: by mDNS, plus the ATEMs that answer the hello but do not
/// advertise. Blocking, for about `timeout`.
pub fn search(timeout: Duration) -> Vec<DiscoveredDevice> {
    let sweep = std::thread::spawn(move || sweep_atems(&own_networks(), timeout));
    let mut found = browse(timeout);
    for ip in sweep.join().unwrap_or_default() {
        if !found.iter().any(|d| d.address == IpAddr::V4(ip)) {
            info!(address = %ip, "ATEM answered the hello but did not advertise");
            found.push(DiscoveredDevice {
                name: "ATEM switcher".into(),
                model: "found by its address".into(),
                address: IpAddr::V4(ip),
                kind: Some(BmdKind::Atem),
            });
        }
    }
    found.sort_by_key(|d| d.address);
    found
}

/// This computer's IPv4 networks worth sweeping: private, not loopback or link-local.
fn own_networks() -> Vec<Ipv4Addr> {
    let Ok(ifs) = if_addrs::get_if_addrs() else { return Vec::new() };
    let mut own: Vec<Ipv4Addr> = ifs
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) if a.is_private() && !a.is_loopback() && !a.is_link_local() => Some(a),
            _ => None,
        })
        .collect();
    own.sort();
    own.dedup();
    own
}

/// The other 253 addresses of `own`'s /24.
fn neighbours(own: Ipv4Addr) -> impl Iterator<Item = Ipv4Addr> {
    let [a, b, c, _] = own.octets();
    (1..=254u8).map(move |d| Ipv4Addr::new(a, b, c, d)).filter(move |ip| *ip != own)
}

/// An ATEM's answer to the hello: a header (12 bytes or more) with the new-session flag.
fn is_hello_answer(p: &[u8]) -> bool {
    p.len() >= 12 && (p[0] >> 3) & streamit_atem::packet::flag::NEW_SESSION_ID != 0
}

/// Says the ATEM hello to every neighbour of `own` and returns those that answer within
/// `timeout`. The session each answer opens is never used, so the ATEM drops it again.
pub fn sweep_atems(own: &[Ipv4Addr], timeout: Duration) -> Vec<Ipv4Addr> {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            warn!("ATEM sweep could not start: {e}");
            return Vec::new();
        }
    };
    for &me in own {
        for ip in neighbours(me) {
            let to = SocketAddr::new(IpAddr::V4(ip), streamit_atem::ATEM_PORT);
            // An unreachable address is only "nothing there".
            let _ = socket.send_to(&streamit_atem::packet::COMMAND_CONNECT_HELLO, to);
        }
    }
    let mut answered = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 1500];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).is_err() {
            break;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, SocketAddr::V4(from))) if from.port() == streamit_atem::ATEM_PORT && is_hello_answer(&buf[..n]) => {
                if !answered.contains(from.ip()) {
                    answered.push(*from.ip());
                }
            }
            Ok(_) => {}
            // Windows reports an ICMP "port unreachable" from an earlier send as an error here.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(_) => break,
        }
    }
    answered.sort();
    answered
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
    fn a_multiview_is_told_apart_from_a_videohub() {
        let mv = txt(&[("class", "Videohub"), ("model", "Blackmagic MultiView 4")]);
        assert_eq!(kind_from_txt(&mv), Some(BmdKind::MultiView));
        assert_eq!(kind_from_txt(&txt(&[("class", "Videohub"), ("name", "MultiView 4 HD")])), Some(BmdKind::MultiView));
        assert_eq!(kind_from_txt(&txt(&[("class", "MultiView")])), Some(BmdKind::MultiView));
        let hub = txt(&[("class", "Videohub"), ("model", "Smart Videohub 40x40 12G")]);
        assert_eq!(kind_from_txt(&hub), Some(BmdKind::VideoHub));
    }

    #[test]
    fn the_sweep_covers_the_rest_of_the_24() {
        let own = Ipv4Addr::new(192, 168, 4, 20);
        let n: Vec<_> = neighbours(own).collect();
        assert_eq!(n.len(), 253);
        assert!(!n.contains(&own));
        assert_eq!((n[0], n[252]), (Ipv4Addr::new(192, 168, 4, 1), Ipv4Addr::new(192, 168, 4, 254)));
    }

    #[test]
    fn only_a_hello_answer_counts() {
        let answer = [0x10, 0x14, 0x53, 0x1b, 0, 0, 0, 0, 0, 0xd1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0];
        assert!(is_hello_answer(&answer));
        assert!(!is_hello_answer(&answer[..8]));
        assert!(!is_hello_answer(&[0x08, 0x0c, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), "an ack, not a hello answer");
    }

    /// A pretend ATEM on 127.0.0.1:9910 is not possible on every machine, so the sweep's
    /// listening half is checked against an address that never answers.
    #[test]
    fn a_sweep_of_nothing_finds_nothing() {
        assert!(sweep_atems(&[], Duration::from_millis(50)).is_empty());
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
