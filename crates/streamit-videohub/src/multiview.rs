//! Blackmagic MultiView 4 client. It speaks the same block protocol as the VideoHub on TCP
//! 9990; StreamIt reads and sets its four input labels (the names shown under each view)
//! and its solo (full screen) view: the solo source is routing output 4 (0-based), and
//! `CONFIGURATION: Solo enabled` turns it on and off. Inputs are numbered 1..=4 here,
//! 0-based on the wire.
use crate::{INPUT_LABELS, apply_labels_into, format_label};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use streamit_blockproto::{Block, Link, OnBlock, OnReset, Timings};
use streamit_core::devices::{DeviceError, LabelKind, Result};
use streamit_core::routing::VIEW_MV_INPUT;

/// The MultiView 4's views.
pub const VIEWS: u16 = 4;
/// The routing output (0-based) that picks the solo view's input.
const SOLO_OUTPUT: u16 = 4;
const ROUTING: &str = "VIDEO OUTPUT ROUTING";
const CONFIGURATION: &str = "CONFIGURATION";

#[derive(Default)]
struct State {
    /// 1-based input -> label.
    labels: HashMap<u16, String>,
    solo_on: bool,
    /// The solo view's input, 1-based.
    solo_input: Option<u16>,
}

impl State {
    fn apply(&mut self, b: &Block) {
        match b.name.as_str() {
            INPUT_LABELS => apply_labels_into(&mut self.labels, b),
            ROUTING => {
                for line in &b.lines {
                    let mut it = line.split_whitespace().filter_map(|x| x.parse::<u16>().ok());
                    if let (Some(SOLO_OUTPUT), Some(i)) = (it.next(), it.next()) {
                        self.solo_input = Some(i + 1);
                    }
                }
            }
            CONFIGURATION => {
                for line in &b.lines {
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("Solo enabled")
                    {
                        self.solo_on = v.trim().eq_ignore_ascii_case("true");
                    }
                }
            }
            _ => {}
        }
    }
}

pub struct MultiView {
    state: Arc<Mutex<State>>,
    link: Link,
}

impl MultiView {
    /// Starts the client. It does not wait for the link: while the MultiView is unreachable
    /// every call reports `Disconnected`, and it keeps retrying.
    pub fn connect(addr: SocketAddr) -> Arc<MultiView> {
        Self::connect_with(addr, Timings::default())
    }

    pub fn connect_with(addr: SocketAddr, timings: Timings) -> Arc<MultiView> {
        let state: Arc<Mutex<State>> = Arc::default();
        let st = state.clone();
        let on_block: OnBlock = Arc::new(move |b: &Block| st.lock().unwrap_or_else(|e| e.into_inner()).apply(b));
        let st = state.clone();
        let on_reset: OnReset = Arc::new(move || *st.lock().unwrap_or_else(|e| e.into_inner()) = State::default());
        // Synced once the first INPUT LABELS block has arrived.
        let link = Link::spawn("multiview", INPUT_LABELS, addr, timings, on_block, on_reset);
        Arc::new(MultiView { state, link })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The input (1..=4) shown full screen, or `None` while all four are shown.
    pub fn solo(&self) -> Result<Option<u16>> {
        self.link.ensure_usable()?;
        let st = self.state();
        Ok(if st.solo_on { st.solo_input } else { None })
    }

    /// Shows `input` (1..=4) full screen, or all four views again with `None`. The solo
    /// source is set before solo goes on, so the old one never flashes up.
    pub async fn set_solo(&self, input: Option<u16>) -> Result<()> {
        match input {
            Some(n) if n == 0 || n > VIEWS => Err(DeviceError::Other(format!("the MultiView has no input {n}"))),
            Some(n) => {
                let route = format!(
                    "{ROUTING}:
{SOLO_OUTPUT} {}

",
                    n - 1
                );
                self.link.command(route, &format!("the full screen view (input {n})")).await?;
                self.state().solo_input = Some(n);
                self.link
                    .command(
                        format!(
                            "{CONFIGURATION}:
Solo enabled: true

"
                        ),
                        "full screen",
                    )
                    .await?;
                self.state().solo_on = true;
                Ok(())
            }
            None => {
                self.link
                    .command(
                        format!(
                            "{CONFIGURATION}:
Solo enabled: false

"
                        ),
                        "all four views",
                    )
                    .await?;
                self.state().solo_on = false;
                Ok(())
            }
        }
    }

    /// `Ok` while connected and its labels are known.
    pub fn ensure_usable(&self) -> Result<()> {
        self.link.ensure_usable()
    }

    /// The four views' labels as the MultiView reports them; "" where it has none.
    pub fn labels(&self) -> Result<Vec<String>> {
        self.link.ensure_usable()?;
        let st = self.state();
        Ok((1..=VIEWS).map(|v| st.labels.get(&v).cloned().unwrap_or_default()).collect())
    }

    /// Names view `view` (1..=4), kept once the MultiView ACKs.
    pub async fn set_label(&self, view: u16, text: &str) -> Result<()> {
        if view == 0 || view > VIEWS {
            return Err(DeviceError::Other(format!("the MultiView has no view {view}")));
        }
        self.link.command(format_label(LabelKind::Input, view, text), &format!("view {view} label")).await?;
        let text = text.replace(['\r', '\n'], " ").trim().to_string();
        self.state().labels.insert(view, text);
        Ok(())
    }
}

#[async_trait::async_trait]
impl streamit_core::devices::Monitor for MultiView {
    async fn set_solo(&self, input: Option<u16>) -> Result<()> {
        MultiView::set_solo(self, input).await
    }
}

/// The labels each view should have so it names the VideoHub input on it: (MultiView
/// input 1..=4, label) for every view whose current label differs. `dests` is the
/// VideoHub input on each view, in `routing::VIEWS` order (`None` when not fed or not
/// known: left alone); `current` is the labels by MultiView input; `hub_label` names an
/// input.
pub fn labels_to_set(
    dests: &[Option<u16>],
    current: &[String],
    hub_label: impl Fn(u16) -> String,
) -> Vec<(u16, String)> {
    dests
        .iter()
        .zip(VIEW_MV_INPUT)
        .filter_map(|(d, mv_in)| {
            let want = hub_label((*d)?);
            let now = current.get(usize::from(mv_in) - 1).map(String::as_str).unwrap_or("");
            (!want.trim().is_empty() && want.trim() != now.trim()).then(|| (mv_in, want.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_views_whose_label_differs_from_their_input_are_named() {
        let names = |n: u16| match n {
            1 => "Camera 1".to_string(),
            5 => "ProPresenter".to_string(),
            _ => String::new(),
        };
        // By MultiView input: 1 (top left) is right; 3 (top right) is not.
        let current = vec!["Camera 1".to_string(), "x".to_string(), "Old".to_string()];
        let dests = [Some(1), Some(5), None, Some(9)];
        assert_eq!(labels_to_set(&dests, &current, names), vec![(3, "ProPresenter".to_string())]);
    }

    #[test]
    fn each_corner_names_the_multiview_input_shown_there() {
        // The MultiView 4 numbers its inputs down the columns: 1 and 2 on the left.
        let names = |n: u16| format!("Source {n}");
        let dests = [Some(11), Some(12), Some(13), Some(14)]; // TL, TR, BL, BR
        assert_eq!(
            labels_to_set(&dests, &[], names),
            vec![
                (1, "Source 11".to_string()),
                (3, "Source 12".to_string()),
                (2, "Source 13".to_string()),
                (4, "Source 14".to_string()),
            ]
        );
    }
}
