//! The steps Prepare, the countdown and End Service go through, and how far each has got,
//! for the Service screen's list of steps (grey until done, green when done).
use std::time::SystemTime;

/// The stage a step belongs to; the Service screen shows the steps of the one it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepGroup {
    Prepare,
    Countdown,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepId {
    LevelsRead,
    OpeningStill,
    PrepareAudioDown,
    EncoderStarted,
    YouTubeReceiving,
    YouTubeTesting,
    HallCountdown,
    CountdownVideo,
    YouTubePublic,
    ClosingStill,
    EndAudioDown,
    ClosingHold,
    YouTubeEnded,
    EncoderStopped,
}

impl StepId {
    pub const PREPARE: [StepId; 6] = [
        StepId::LevelsRead,
        StepId::OpeningStill,
        StepId::PrepareAudioDown,
        StepId::EncoderStarted,
        StepId::YouTubeReceiving,
        StepId::YouTubeTesting,
    ];
    pub const COUNTDOWN: [StepId; 3] = [StepId::HallCountdown, StepId::CountdownVideo, StepId::YouTubePublic];
    pub const END: [StepId; 5] =
        [StepId::ClosingStill, StepId::EndAudioDown, StepId::ClosingHold, StepId::YouTubeEnded, StepId::EncoderStopped];

    pub fn group(self) -> StepGroup {
        use StepId::*;
        match self {
            LevelsRead | OpeningStill | PrepareAudioDown | EncoderStarted | YouTubeReceiving | YouTubeTesting => {
                StepGroup::Prepare
            }
            HallCountdown | CountdownVideo | YouTubePublic => StepGroup::Countdown,
            ClosingStill | EndAudioDown | ClosingHold | YouTubeEnded | EncoderStopped => StepGroup::End,
        }
    }

    pub fn label(self) -> &'static str {
        use StepId::*;
        match self {
            LevelsRead => "Stream audio levels read",
            OpeningStill => "Opening still on air",
            PrepareAudioDown | EndAudioDown => "Stream audio faded down",
            EncoderStarted => "Web Presenter streaming",
            YouTubeReceiving => "YouTube receiving",
            YouTubeTesting => "YouTube test started",
            HallCountdown => "Hall screens to the countdown",
            CountdownVideo => "Countdown video playing",
            YouTubePublic => "YouTube public",
            ClosingStill => "Closing still on air",
            ClosingHold => "Closing hold",
            YouTubeEnded => "YouTube broadcast ended",
            EncoderStopped => "Web Presenter stopped",
        }
    }
}

impl StepGroup {
    pub fn ids(self) -> &'static [StepId] {
        match self {
            StepGroup::Prepare => &StepId::PREPARE,
            StepGroup::Countdown => &StepId::COUNTDOWN,
            StepGroup::End => &StepId::END,
        }
    }
}

/// How far a step has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepMark {
    Todo,
    /// Happening now, with what it is waiting on, if anything worth saying.
    Now(Option<String>),
    Done(SystemTime),
    Failed(String),
}

impl StepMark {
    pub fn done() -> Self {
        StepMark::Done(SystemTime::now())
    }

    /// Done or Failed with the error.
    pub fn of<T, E: std::fmt::Display>(r: &Result<T, E>) -> Self {
        match r {
            Ok(_) => StepMark::done(),
            Err(e) => StepMark::Failed(e.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub id: StepId,
    pub mark: StepMark,
}

/// A group's steps, none of them started.
pub fn fresh(group: StepGroup) -> Vec<Step> {
    group.ids().iter().map(|&id| Step { id, mark: StepMark::Todo }).collect()
}

/// `steps` with `group`'s steps put back to not started.
pub fn reset(steps: &mut Vec<Step>, group: StepGroup) {
    steps.retain(|s| s.id.group() != group);
    steps.extend(fresh(group));
}

pub fn mark(steps: &mut [Step], id: StepId, m: StepMark) {
    if let Some(s) = steps.iter_mut().find(|s| s.id == id) {
        s.mark = m;
    }
}

/// `group`'s steps as they stand, all not started when it has not run.
pub fn of_group(steps: &[Step], group: StepGroup) -> Vec<Step> {
    let mine: Vec<Step> = steps.iter().filter(|s| s.id.group() == group).cloned().collect();
    if mine.is_empty() { fresh(group) } else { mine }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_is_in_its_own_group_once() {
        for g in [StepGroup::Prepare, StepGroup::Countdown, StepGroup::End] {
            for id in g.ids() {
                assert_eq!(id.group(), g);
            }
        }
    }

    #[test]
    fn reset_clears_only_that_group_and_of_group_fills_in_missing() {
        let mut s = fresh(StepGroup::Prepare);
        mark(&mut s, StepId::LevelsRead, StepMark::done());
        assert_eq!(of_group(&s, StepGroup::Countdown), fresh(StepGroup::Countdown));
        reset(&mut s, StepGroup::Countdown);
        mark(&mut s, StepId::HallCountdown, StepMark::Failed("x".into()));
        reset(&mut s, StepGroup::End);
        assert!(matches!(of_group(&s, StepGroup::Prepare)[0].mark, StepMark::Done(_)));
        assert_eq!(of_group(&s, StepGroup::Countdown)[0].mark, StepMark::Failed("x".into()));
        reset(&mut s, StepGroup::Prepare);
        assert_eq!(of_group(&s, StepGroup::Prepare), fresh(StepGroup::Prepare));
        assert_eq!(s.len(), 14);
    }
}
