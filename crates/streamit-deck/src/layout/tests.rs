use super::*;
use std::path::PathBuf;
use streamit_core::engine::TestPlay;
use streamit_core::rollin::{Cue, Destination};
use streamit_core::routing::{Preset, RoutingState, TargetState};
use streamit_core::scenes::Scene;

fn cue(name: &str) -> Cue {
    Cue {
        path: PathBuf::from(format!("{name}.mp4")),
        name: name.into(),
        destination: Destination::HallAndOnline,
        gain_db: 0.0,
        return_input: None,
        scene: None,
    }
}

fn with_cues(n: usize, stage: Stage) -> Snapshot {
    Snapshot {
        stage,
        cues: (0..n).map(|i| (cue(&format!("Video {i}")), CueState::Cued)).collect(),
        ..Snapshot::default()
    }
}

fn live(n: usize) -> Snapshot {
    with_cues(n, Stage::Live)
}

fn cmd(page: Page, key: u8, s: &Snapshot) -> Option<Command> {
    match press_for(page, key, s) {
        Some(Press::Command(c)) => Some(c),
        _ => None,
    }
}

fn scenes(n: u32) -> Vec<Scene> {
    (1..=n).map(|id| Scene { id, name: format!("Scene {id}"), key: String::new(), screens: None, cue: None }).collect()
}

const CD: Stage = Stage::Countdown { remaining: Duration::from_secs(42), public: false };

// ---- the top row

#[test]
fn the_top_row_opens_every_page_from_every_page() {
    let s = Snapshot::default();
    let all = [
        Page::Service,
        Page::Videos,
        Page::Scenes,
        Page::Routing(Target::MultiView),
        Page::Routing(Target::StreamAtem),
        Page::Routing(Target::ScreensAtem),
    ];
    for page in all {
        assert_eq!(press_for(page, 0, &s), Some(Press::Go(Page::Service)), "{page:?}");
        assert_eq!(press_for(page, 1, &s), Some(Press::Go(Page::Videos)));
        assert_eq!(press_for(page, 2, &s), Some(Press::Go(Page::Scenes)));
        assert_eq!(appearance(page, 0, &s, true).text, "SERVICE");
        assert_eq!(appearance(page, 3, &s, true).text, "ROUTING");
        assert_eq!(press_for(page, PHASE_KEY, &s), None);
    }
    // Routing opens on MultiView from elsewhere, and stays on its target from Routing.
    assert_eq!(press_for(Page::Service, 3, &s), Some(Press::Go(Page::Routing(Target::MultiView))));
    let sa = Page::Routing(Target::StreamAtem);
    assert_eq!(press_for(sa, 3, &s), Some(Press::Go(sa)));
    assert_eq!(Page::default(), Page::Service);
}

#[test]
fn the_page_shown_is_lit() {
    let s = Snapshot::default();
    assert_eq!(appearance(Page::Videos, 1, &s, true).bg, NAV_ON);
    assert_eq!(appearance(Page::Videos, 0, &s, true).bg, NAV);
    assert_eq!(appearance(Page::Routing(Target::ScreensAtem), 3, &s, true).bg, NAV_ON);
}

#[test]
fn routing_picks_its_target_in_the_top_row() {
    let s = Snapshot::default();
    let mv = Page::Routing(Target::MultiView);
    assert_eq!(press_for(mv, 4, &s), Some(Press::Go(Page::Routing(Target::MultiView))));
    assert_eq!(press_for(mv, 5, &s), Some(Press::Go(Page::Routing(Target::StreamAtem))));
    assert_eq!(press_for(mv, 6, &s), Some(Press::Go(Page::Routing(Target::ScreensAtem))));
    let l = appearance(Page::Routing(Target::StreamAtem), 5, &s, true);
    assert_eq!((l.text.as_str(), l.bg), ("STREAM\nATEM", SUB_ON));
    assert_eq!(appearance(Page::Routing(Target::StreamAtem), 4, &s, true).bg, SUB);
    // Elsewhere those keys are empty and do nothing.
    for k in 4..7 {
        assert_eq!(press_for(Page::Service, k, &s), None);
        assert_eq!(appearance(Page::Videos, k, &s, true), blank());
    }
}

#[test]
fn no_key_shows_device_health() {
    let s = Snapshot::default();
    for page in [Page::Service, Page::Videos, Page::Scenes, Page::Routing(Target::MultiView)] {
        for k in 0..KEYS {
            let t = appearance(page, k, &s, true).text;
            assert!(!t.contains("ATEM\n") || k >= 4, "{page:?} key {k}: {t}");
            assert!(!["VIDEO\nHUB", "YOUTUBE", "DECKLINK", "NDI"].contains(&t.as_str()), "{page:?} key {k}: {t}");
        }
    }
}

#[test]
fn the_videos_key_shows_a_playing_video_from_other_pages() {
    let mut s = live(1);
    assert_eq!(appearance(Page::Service, 1, &s, true).text, "VIDEOS");
    s.playing = Some(0);
    let l = appearance(Page::Service, 1, &s, true);
    assert_eq!((l.text.as_str(), l.bg), ("VIDEOS\nPLAYING", RED));
    assert_eq!(appearance(Page::Videos, 1, &s, true).text, "VIDEOS");
}

// ---- the stage key

#[test]
fn phase_key_shows_each_stage() {
    let mut s = Snapshot::default();
    let look_of = |s: &Snapshot| appearance(Page::Scenes, 7, s, true);
    assert_eq!(look_of(&s).text, "IDLE");
    s.stage = Stage::Prepared(Readiness::Ready);
    assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("READY", GREEN));
    s.stage = Stage::Prepared(Readiness::Waiting("x".into()));
    assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("WAIT", AMBER));
    s.stage = CD;
    assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("T-0:42", AMBER));
    s.stage = Stage::Countdown { remaining: Duration::from_secs(9), public: true };
    assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("T-0:09", RED));
    s.stage = Stage::Live;
    assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("LIVE", RED));
    s.on_air_since = Some(SystemTime::now() - Duration::from_secs(3725));
    assert_eq!(look_of(&s).text, "LIVE\n1:02:05");
    s.stage = Stage::Ending { remaining: Duration::from_secs(7) };
    assert_eq!(look_of(&s).text, "END 7");
    s.stage = Stage::Ended;
    assert_eq!(look_of(&s).text, "ENDED");
    s.simulation = true;
    assert_eq!(look_of(&s).text, "ENDED\nSIM");
}

// ---- Service, by stage

#[test]
fn before_the_service_prepare_and_start_now_which_asks_first() {
    let s = Snapshot::default();
    assert_eq!(cmd(Page::Service, 8, &s), Some(Command::Prepare));
    assert_eq!(press_for(Page::Service, 11, &s), Some(Press::Ask(Asked::StartNow)));
    assert_eq!(press_for(Page::Service, CONFIRM_START, &s), Some(Press::Confirm(Asked::StartNow)));
    assert_eq!(Asked::StartNow.command(), Command::GoLive);
    assert_eq!(appearance(Page::Service, CONFIRM_START, &s, true), blank(), "only while asking");
    let on = appearance_with(Page::Service, CONFIRM_START, &s, true, None, Some(Asked::StartNow));
    assert_eq!((on.text.as_str(), on.bg, on.blink), ("CONFIRM\nSTART", RED, true));
    assert_eq!(cmd(Page::Service, 9, &s), None, "no countdown before Prepare");
    // Recover and End broadcast only when offered.
    assert_eq!(cmd(Page::Service, 15, &s), None);
    assert_eq!(appearance(Page::Service, 15, &s, true), blank());
    let s = Snapshot { offers: vec![Offer::Recover, Offer::EndBroadcast], ..s };
    assert_eq!(cmd(Page::Service, 15, &s), Some(Command::Recover));
    assert_eq!(cmd(Page::Service, 14, &s), Some(Command::EndBroadcast));
    // After a service has ended: Prepare for the next one, no Start service now.
    let ended = Snapshot { stage: Stage::Ended, ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 8, &ended), Some(Command::Prepare));
    assert_eq!(press_for(Page::Service, 11, &ended), None);
}

#[test]
fn prepared_offers_the_countdown_and_start_now_at_once() {
    let s = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 8, &s), Some(Command::Prepare));
    assert_eq!(appearance(Page::Service, 8, &s, true).text, "PREPARE\nAGAIN");
    assert_eq!(cmd(Page::Service, 9, &s), Some(Command::Countdown));
    assert_eq!(cmd(Page::Service, 11, &s), Some(Command::GoLive), "no confirm once prepared");
}

#[test]
fn with_the_countdown_off_its_key_is_blank_and_does_nothing() {
    let s = Snapshot { stage: Stage::Prepared(Readiness::Ready), countdown_off: true, ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 9, &s), None);
    assert_eq!(appearance(Page::Service, 9, &s, true).text, "");
    assert_eq!(cmd(Page::Service, 11, &s), Some(Command::GoLive));
}

#[test]
fn abandon_asks_first_while_prepared_or_counting_down_and_youtube_is_private() {
    for stage in [Stage::Prepared(Readiness::Ready), CD] {
        let s = Snapshot { stage, ..Snapshot::default() };
        assert_eq!(press_for(Page::Service, ABANDON, &s), Some(Press::Ask(Asked::Abandon)));
        assert_eq!(appearance(Page::Service, ABANDON, &s, true).text, "ABANDON");
        assert_eq!(press_for(Page::Service, CONFIRM_ABANDON, &s), Some(Press::Confirm(Asked::Abandon)));
        let on = appearance_with(Page::Service, CONFIRM_ABANDON, &s, true, None, Some(Asked::Abandon));
        assert_eq!((on.text.as_str(), on.blink), ("CONFIRM\nABANDON", true));
        // Going public: no abandoning, and no stopping the countdown.
        let public = Snapshot { public_started: true, ..s };
        assert_eq!(press_for(Page::Service, ABANDON, &public), None);
        assert_eq!(appearance(Page::Service, ABANDON, &public, true), blank());
    }
    assert_eq!(Asked::Abandon.command(), Command::Abandon);
    let going = Snapshot { stage: CD, public_started: true, ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 9, &going), None);
    assert_eq!(appearance(Page::Service, 9, &going, true), blank());
    // Not before the service.
    assert_eq!(press_for(Page::Service, ABANDON, &Snapshot::default()), None);
}

#[test]
fn the_countdown_page_starts_now_stops_or_goes_public() {
    let s = Snapshot { stage: CD, ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 8, &s), Some(Command::GoLive));
    assert_eq!(cmd(Page::Service, 9, &s), Some(Command::StopCountdown));
    assert_eq!(cmd(Page::Service, 10, &s), Some(Command::GoPublic));
    assert_eq!(appearance(Page::Service, 10, &s, true).text, "GO\nPUBLIC\nNOW");
    let public = Snapshot { stage: Stage::Countdown { remaining: Duration::from_secs(5), public: true }, ..s };
    assert_eq!(appearance(Page::Service, 10, &public, true).text, "PUBLIC");
    assert_eq!(cmd(Page::Service, 11, &public), None);
}

#[test]
fn live_has_the_videos_the_scenes_and_end_service() {
    let mut s = live(14);
    s.scenes = scenes(7);
    assert_eq!(cmd(Page::Service, 8, &s), Some(Command::PlayCue(0)));
    assert_eq!(cmd(Page::Service, 22, &s), Some(Command::PlayCue(13)));
    assert_eq!(cmd(Page::Service, 24, &s), Some(Command::GoScene(1)));
    assert_eq!(cmd(Page::Service, 28, &s), Some(Command::GoScene(5)));
    assert_eq!(appearance(Page::Service, 28, &s, true).text, "SCENE 5");
    assert_eq!(press_for(Page::Service, 30, &s), Some(Press::Ask(Asked::EndService)));
    assert_eq!(press_for(Page::Service, CONFIRM_END, &s), Some(Press::Confirm(Asked::EndService)));
    let on = appearance_with(Page::Service, CONFIRM_END, &s, false, None, Some(Asked::EndService));
    assert_eq!((on.text.as_str(), on.bg, on.blink), ("CONFIRM\nEND", DIM_BG, true));
    // The spare key shows whatever is offered, Recover first.
    assert_eq!(cmd(Page::Service, 29, &s), None);
    s.offers = vec![Offer::GoPublic, Offer::Recover];
    assert_eq!(cmd(Page::Service, 29, &s), Some(Command::Recover));
    s.offers = vec![Offer::GoPublic];
    assert_eq!(appearance(Page::Service, 29, &s, true).text, "RETRY\nPUBLIC");
    // Already ending: END SERVICE (pressed on the ending page) skips the hold at once.
    let running = Snapshot { job: Some("End Service"), ..s };
    assert_eq!(cmd(Page::Service, 30, &running), Some(Command::EndService));
}

#[test]
fn ending_skips_the_hold() {
    let s = Snapshot { stage: Stage::Ending { remaining: Duration::from_secs(5) }, ..Snapshot::default() };
    assert_eq!(cmd(Page::Service, 8, &s), Some(Command::EndService));
    assert_eq!(appearance(Page::Service, 8, &s, true).text, "SKIP\nHOLD");
    assert_eq!(cmd(Page::Service, 9, &s), Some(Command::EndBroadcast));
}

#[test]
fn flash_key_blinks_start_now_and_recover() {
    let mut s = Snapshot {
        stage: Stage::Prepared(Readiness::Ready),
        offers: vec![Offer::Recover],
        flash_key: Some(FlashKey::GoLive),
        ..Snapshot::default()
    };
    assert!(appearance(Page::Service, 11, &s, true).blink);
    assert_eq!(appearance(Page::Service, 11, &s, false).bg, DIM_BG);
    assert!(!appearance(Page::Service, 15, &s, false).blink);
    s.flash_key = Some(FlashKey::Recover);
    assert!(appearance(Page::Service, 15, &s, false).blink);
}

#[test]
fn a_confirm_shows_only_on_the_service_page() {
    let s = live(14);
    let l = appearance_with(Page::Videos, CONFIRM_END, &s, true, None, Some(Asked::EndService));
    assert_ne!(l.text, "CONFIRM\nEND");
}

// ---- Videos

#[test]
fn video_keys_play_once_the_countdown_starts_and_test_before() {
    let s = with_cues(14, CD);
    assert_eq!(cmd(Page::Videos, 8, &s), Some(Command::PlayCue(0)));
    assert_eq!(cmd(Page::Videos, 14, &s), Some(Command::PlayCue(6)));
    assert_eq!(cmd(Page::Videos, 16, &s), Some(Command::PlayCue(7)));
    assert_eq!(cmd(Page::Videos, 22, &s), Some(Command::PlayCue(13)));
    for i in 0..14 {
        assert_eq!(cmd(Page::Videos, cue_key_number(i) - 1, &s), Some(Command::PlayCue(i)), "cue {i}");
    }
    let before = with_cues(2, Stage::Prepared(Readiness::Ready));
    assert_eq!(cmd(Page::Videos, 9, &before), Some(Command::TestVideo(before.cues[1].0.clone())));
    assert_eq!(cmd(Page::Videos, 10, &before), None, "empty slot");
    assert_eq!(appearance(Page::Videos, 10, &before, true), blank());
}

#[test]
fn a_test_play_shows_on_its_key_and_stop_stops_it() {
    let mut s = with_cues(2, Stage::Idle);
    assert_eq!(cmd(Page::Videos, 15, &s), None);
    s.test = Some(TestPlay { cue: s.cues[1].0.clone(), position: Duration::ZERO, duration: Duration::ZERO });
    let l = appearance(Page::Videos, 9, &s, true);
    assert_eq!((l.text.as_str(), l.bg), ("TEST\nVideo 1", AMBER));
    assert_eq!(appearance(Page::Videos, 8, &s, true).bg, BLUE);
    assert_eq!(cmd(Page::Videos, 15, &s), Some(Command::StopVideo));
    assert_eq!(appearance(Page::Videos, 15, &s, true).bg, RED);
}

#[test]
fn cue_colours_and_blinking() {
    let mut s = live(4);
    s.cues[0].1 = CueState::Playing;
    s.cues[1].1 = CueState::Played;
    s.cues[2].1 = CueState::Error("e".into());
    assert_eq!(appearance(Page::Videos, 8, &s, true).bg, RED);
    assert_eq!(appearance(Page::Videos, 9, &s, true).bg, GREY);
    assert_eq!(appearance(Page::Videos, 11, &s, true).bg, BLUE);
    let on = appearance(Page::Videos, 10, &s, true);
    let off = appearance(Page::Videos, 10, &s, false);
    assert!(on.blink && off.blink);
    assert_eq!(off.fg, off.bg, "error text blinks off");
    s.flash = Some(3);
    let f = appearance(Page::Videos, 11, &s, false);
    assert!(f.blink);
    assert_eq!(f.bg, DIM_BG);
}

#[test]
fn stop_video_and_stream_audio_up() {
    let mut s = live(1);
    assert_eq!(cmd(Page::Videos, 15, &s), None);
    assert_eq!(appearance(Page::Videos, 15, &s, true).bg, DIM_BG);
    s.playing = Some(0);
    assert_eq!(cmd(Page::Videos, 15, &s), Some(Command::StopVideo));
    assert_eq!(cmd(Page::Service, 15, &s), Some(Command::StopVideo), "on the live page too");
    assert_eq!(cmd(Page::Videos, 23, &s), None, "not ducked");
    s.mics_ducked = true;
    assert_eq!(cmd(Page::Videos, 23, &s), Some(Command::UnmuteMics));
    assert_eq!(appearance(Page::Videos, 23, &s, true).bg, AMBER);
    // Ducked but no video (the holding still): STREAM AUDIO UP is inert.
    s.playing = None;
    assert_eq!(cmd(Page::Videos, 23, &s), None);
}

// ---- Scenes

#[test]
fn the_scenes_page_goes_to_a_scene_and_lights_the_one_on_now() {
    let mut s = Snapshot { scenes: scenes(13), ..Snapshot::default() };
    s.scenes[1].key = "PREACH".into();
    assert_eq!(cmd(Page::Scenes, 8, &s), Some(Command::GoScene(1)));
    assert_eq!(cmd(Page::Scenes, 19, &s), Some(Command::GoScene(12)));
    assert_eq!(press_for(Page::Scenes, 20, &s), None, "twelve at most");
    assert_eq!(appearance(Page::Scenes, 9, &s, true).text, "PREACH");
    assert_eq!(appearance(Page::Scenes, 9, &s, true).bg, SCENE_BG);
    s.active_scene = Some(2);
    assert_eq!(appearance(Page::Scenes, 9, &s, true).bg, AMBER);
    s.scenes.truncate(1);
    assert_eq!(appearance(Page::Scenes, 9, &s, true), blank());
    assert_eq!(press_for(Page::Scenes, 9, &s), None);
}

// ---- Routing

fn rt_snap() -> Snapshot {
    let mut routing = RoutingState {
        labels: vec!["Cam 1".into(), "Cam 2".into(), "Choir".into()],
        media: Some(7),
        ..RoutingState::default()
    };
    routing.targets[0] = TargetState {
        outputs: vec![37, 38, 39, 40],
        dests: vec![Some(1), Some(2), Some(3), None],
        presets: vec![
            Preset { slot: 1, name: "Service".into(), inputs: vec![1, 2, 3, 4] },
            Preset { slot: 3, name: "Cameras".into(), inputs: vec![5, 6, 7, 8] },
        ],
        program: None,
        preview: None,
        stills: vec![],
    };
    routing.targets[1] = TargetState {
        outputs: vec![1, 2, 3, 4, 0, 0, 0, 11],
        dests: vec![Some(1), Some(2), Some(3), Some(4), None, None, None, Some(13)],
        presets: vec![
            Preset { slot: 1, name: "Normal".into(), inputs: vec![1, 2, 3, 4, 0, 0, 0, 13] },
            Preset { slot: 2, name: "Band".into(), inputs: vec![1, 25, 3, 4, 0, 0, 0, 13] },
            Preset { slot: 4, name: "Lectern".into(), inputs: vec![1, 2, 21, 4, 0, 0, 0, 13] },
        ],
        program: Some(2),
        preview: Some(3),
        stills: vec![],
    };
    Snapshot { routing, ..Snapshot::default() }
}

#[test]
fn preset_keys_recall_saved_presets_only() {
    let s = rt_snap();
    let mv = Page::Routing(Target::MultiView);
    assert_eq!(press_for(mv, 8, &s), Some(Press::Recall(Target::MultiView, 1)));
    assert_eq!(press_for(mv, 10, &s), Some(Press::Recall(Target::MultiView, 3)));
    assert_eq!(press_for(mv, 9, &s), None, "slot 2 is empty");
    assert_eq!(press_for(mv, 15, &s), None, "slot 8 is empty");
    assert_eq!(appearance(mv, 9, &s, true).text, "2\nEMPTY");
    assert_eq!(appearance(mv, 8, &s, true).text, "SERVICE");
    assert_eq!(press_for(Page::Routing(Target::StreamAtem), 9, &s), Some(Press::Recall(Target::StreamAtem, 2)));
}

#[test]
fn the_preset_on_the_destinations_now_is_green() {
    let mut s = rt_snap();
    let mv = Page::Routing(Target::MultiView);
    assert_eq!(appearance(mv, 8, &s, true).bg, SLATE, "view 4 is not known yet");
    s.routing.targets[0].dests[3] = Some(4);
    assert_eq!(appearance(mv, 8, &s, true).bg, GREEN);
    assert_eq!(appearance(mv, 10, &s, true).bg, SLATE);
    assert_eq!(appearance(Page::Routing(Target::StreamAtem), 8, &s, true).bg, GREEN, "Normal is up");
}

#[test]
fn view_keys_show_what_each_view_has_on_it() {
    let s = rt_snap();
    let mv = Page::Routing(Target::MultiView);
    let l = appearance(mv, 19, &s, true);
    assert_eq!((l.text.as_str(), l.bg), ("TL\nCAM 1", VIEW_COLOURS[0]));
    assert_eq!(appearance(mv, 20, &s, true).text, "TR\nCAM 2");
    assert_eq!(appearance(mv, 27, &s, true).text, "BL\nCHOIR");
    assert_eq!(appearance(mv, 28, &s, true).text, "BR\n?", "not known");
    for k in [19, 20, 27, 28] {
        assert_eq!(press_for(mv, k, &s), None, "the view keys only show");
    }
}

#[test]
fn atem_input_keys_show_program_preview_and_unfed_inputs() {
    let mut s = rt_snap();
    let page = Page::Routing(Target::StreamAtem);
    let l = appearance(page, 17, &s, true);
    assert_eq!((l.text.as_str(), l.bg), ("IN 2\nCAM 2", RED), "input 2 is on program");
    assert_eq!(appearance(page, 18, &s, true).bg, GREEN, "input 3 is on preview");
    assert_eq!(appearance(page, 16, &s, true).bg, STREAM_INPUT_BG);
    assert_ne!(STREAM_INPUT_BG, SCREENS_INPUT_BG);
    assert_eq!(appearance(page, 20, &s, true).text, "IN 5\n—", "not fed by the router");
    assert_eq!(appearance(page, 23, &s, true).text, "IN 8\nINPUT 13");
    s.job = Some("a video");
    assert_eq!(appearance(page, 23, &s, true).text, "IN 8\nLOCKED");
    for k in 16..24 {
        assert_eq!(press_for(page, k, &s), None, "the input keys only show");
    }
}

#[test]
fn a_preset_that_changes_the_on_air_input_needs_a_second_press() {
    let s = rt_snap();
    assert!(needs_second_press(&s, Target::StreamAtem, 2), "Band changes input 2, on program");
    assert!(!needs_second_press(&s, Target::StreamAtem, 4), "Lectern changes input 3, on preview");
    assert!(!needs_second_press(&s, Target::MultiView, 3), "the MultiView is never on air");
    assert!(!needs_second_press(&s, Target::StreamAtem, 7), "empty slot");
    let page = Page::Routing(Target::StreamAtem);
    let on = appearance_with(page, 9, &s, true, Some((Target::StreamAtem, 2)), None);
    assert_eq!((on.text.as_str(), on.bg, on.blink), ("PRESS\nAGAIN", AMBER, true));
    assert_eq!(appearance_with(page, 9, &s, false, Some((Target::StreamAtem, 2)), None).bg, DIM_BG);
    assert_eq!(appearance_with(page, 9, &s, true, Some((Target::ScreensAtem, 2)), None).text, "BAND");
}

#[test]
fn cue_names_wrap_to_three_lines_with_ellipsis() {
    assert_eq!(wrap_cue_name("Welcome", 9), "Welcome");
    assert_eq!(wrap_cue_name("Welcome to church", 9), "Welcome\nto church");
    let w = wrap_cue_name("one two three four five six seven eight nine ten", 9);
    assert_eq!(w.lines().count(), 3);
    assert!(w.ends_with('…'));
    assert!(w.lines().all(|l| l.chars().count() <= 9));
    let long = wrap_cue_name("Supercalifragilisticexpialidocious", 9);
    assert!(long.lines().count() <= 3 && long.ends_with('…'));
}
