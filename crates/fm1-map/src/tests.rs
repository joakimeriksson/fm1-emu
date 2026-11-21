use super::*;

fn mpk_profile() -> Profile {
    builtin().into_iter().find(|p| p.name == "Akai MPK mini 3").expect("the MPK profile parses")
}

/// The MPK profile with its keys on the panel and MIDI on the channels played.
fn mpk() -> Mapper {
    let mut p = mpk_profile();
    p.keys_mode = KeysMode::Panel;
    p.midi_ch = 0;
    Mapper::new(p)
}

#[test]
fn mpk_keys_play_at_their_pitch_on_the_selected_track() {
    // as shipped: auto, MIDI IN on the channel played (1), so firmwares that listen on 1 hear it
    let mut m = Mapper::new(mpk_profile());
    assert_eq!(m.keys_mode(), KeysMode::Auto);
    assert_eq!(run(&mut m, &[&[0x90, 48, 90]]), vec![Out::Midi { bytes: [0x90, 48, 90], len: 3 }]);
    // with midi_ch 16 (SLOOP: the selected track)
    let mut p = mpk_profile();
    p.midi_ch = 16;
    let mut m = Mapper::new(p);
    // a C major scale from C3 goes up, note for note, on channel 16
    let v = run(&mut m, &[&[0x90, 48, 90], &[0x80, 48, 0], &[0x90, 52, 90], &[0x90, 53, 90], &[0xe0, 0, 80]]);
    assert_eq!(v, vec![
        Out::Midi { bytes: [0x9f, 48, 90], len: 3 },
        Out::Midi { bytes: [0x8f, 48, 0], len: 3 },
        Out::Midi { bytes: [0x9f, 52, 90], len: 3 },
        Out::Midi { bytes: [0x9f, 53, 90], len: 3 },
        Out::Midi { bytes: [0xef, 0, 80], len: 3 },
    ]);
    // a note held when the keys go to the panel is let go on channel 16 too
    let mut v = Vec::new();
    m.set_keys_mode(KeysMode::Panel, &mut |o| v.push(o));
    assert!(v.contains(&Out::Midi { bytes: [0x8f, 52, 0], len: 3 }), "{v:?}");
    // while a function pad (GLO, A8) is held, auto plays the panel's keys
    let mut m = Mapper::new(mpk_profile());
    let v = run(&mut m, &[&[0x99, 43, 100], &[0x90, 60, 100]]);
    assert_eq!(v, vec![Out::Key { id: 7, down: true, vel: 0 }, Out::Key { id: KEY0 + 7, down: true, vel: 100 }]);
}

fn run(m: &mut Mapper, msgs: &[&[u8]]) -> Vec<Out> {
    let mut v = Vec::new();
    for msg in msgs {
        m.feed(msg, &mut |o| v.push(o));
    }
    v
}

#[test]
fn builtins_parse_and_round_trip() {
    let b = builtin();
    assert_eq!(b.len(), 2, "every built-in profile parses");
    for p in b {
        let again = Profile::from_json(&p.to_json()).unwrap();
        assert_eq!(again, p);
    }
}

#[test]
fn targets_round_trip_through_their_names() {
    for t in Target::all() {
        assert_eq!(t.to_string().parse::<Target>().unwrap(), t, "{t}");
    }
    assert_eq!("key:F3".parse::<Target>().unwrap(), Target::Key(0));
    assert_eq!("key:G5".parse::<Target>().unwrap(), Target::Key(26));
    assert!("NOPE".parse::<Target>().is_err());
}

#[test]
fn keys_play_the_panel_keys_with_velocity() {
    let mut m = mpk();
    // F3 (53) is the panel's first key; C4 (60) its 8th
    let v = run(&mut m, &[&[0x90, 53, 90], &[0x90, 60, 100], &[0x80, 53, 0], &[0x90, 60, 0]]);
    assert_eq!(v, vec![
        Out::Key { id: KEY0, down: true, vel: 90 },
        Out::Key { id: KEY0 + 7, down: true, vel: 100 },
        Out::Key { id: KEY0, down: false, vel: 0 },
        Out::Key { id: KEY0 + 7, down: false, vel: 0 },
    ]);
}

#[test]
fn notes_beyond_the_panel_fold_by_octaves_and_share_a_key() {
    let mut m = mpk();
    // C3 (48) folds up to C4, the key C4 itself plays: one press, released when both are
    let v = run(&mut m, &[&[0x90, 48, 64], &[0x90, 60, 64], &[0x80, 48, 0], &[0x80, 60, 0]]);
    assert_eq!(v, vec![Out::Key { id: KEY0 + 7, down: true, vel: 64 }, Out::Key { id: KEY0 + 7, down: false, vel: 0 }]);
}

#[test]
fn pads_hold_buttons_and_knobs_turn_encoders() {
    let mut m = mpk();
    let v = run(&mut m, &[&[0x99, 43, 100], &[0x89, 43, 0]]);
    assert_eq!(v, vec![Out::Key { id: 7, down: true, vel: 0 }, Out::Key { id: 7, down: false, vel: 0 }], "pad A8 is GLO");
    // absolute: the first value only sets where the knob is, then each change turns
    let v = run(&mut m, &[&[0xb0, 70, 10], &[0xb0, 70, 13], &[0xb0, 70, 11]]);
    assert_eq!(v, vec![Out::Enc { enc: 2, clicks: 3 }, Out::Enc { enc: 2, clicks: -2 }]);
    let v = run(&mut m, &[&[0xb0, 77, 127]]);
    assert_eq!(v, vec![Out::Adc { ch: POT_ADC, value: 1023 }], "knob 8 is MASTER");
}

#[test]
fn relative_encodings() {
    let mut p = Profile { name: "t".into(), ..Profile::default() };
    for (cc, mode) in [(1, Mode::Relative), (2, Mode::Relative64), (3, Mode::RelativeSigned)] {
        let mut b = Binding::new(Src { cc: Some(cc), ..Src::default() }, Target::Enc(2));
        b.mode = mode;
        p.bindings.push(b);
    }
    let mut m = Mapper::new(p);
    let v = run(&mut m, &[&[0xb0, 1, 1], &[0xb0, 1, 127], &[0xb0, 2, 66], &[0xb0, 2, 63], &[0xb0, 3, 2], &[0xb0, 3, 65]]);
    let clicks: Vec<i32> = v.iter().map(|o| match o { Out::Enc { clicks, .. } => *clicks, _ => 0 }).collect();
    assert_eq!(clicks, vec![1, -1, 2, -1, 2, -1]);
}

#[test]
fn a_scale_below_one_keeps_the_fraction() {
    let mut b = Binding::new(Src { cc: Some(1), ..Src::default() }, Target::Enc(0));
    b.mode = Mode::Relative;
    b.scale = 0.5;
    let mut m = Mapper::new(Profile { name: "t".into(), bindings: vec![b], ..Profile::default() });
    let v = run(&mut m, &[&[0xb0, 1, 1], &[0xb0, 1, 1], &[0xb0, 1, 1]]);
    assert_eq!(v, vec![Out::Enc { enc: 0, clicks: 1 }]);
}

#[test]
fn unmapped_messages_pass_to_the_midi_jack() {
    let mut m = mpk();
    let v = run(&mut m, &[&[0xe0, 0, 80], &[0xb0, 64, 127], &[0xf8]]);
    assert_eq!(v, vec![
        Out::Midi { bytes: [0xe0, 0, 80], len: 3 },
        Out::Midi { bytes: [0xb0, 64, 127], len: 3 },
        Out::Midi { bytes: [0xf8, 0, 0], len: 1 },
    ]);
}

#[test]
fn keys_mode_cycles_and_auto_follows_the_function_buttons() {
    let mut m = mpk();
    // pad B3: panel -> MIDI
    let v = run(&mut m, &[&[0x99, 46, 100], &[0x89, 46, 0]]);
    assert_eq!(v, vec![Out::KeysMode(KeysMode::Midi)]);
    let v = run(&mut m, &[&[0x90, 60, 100], &[0x80, 60, 0]]);
    assert_eq!(v, vec![Out::Midi { bytes: [0x90, 60, 100], len: 3 }, Out::Midi { bytes: [0x80, 60, 0], len: 3 }]);
    // MIDI -> auto: MIDI unless GLO (pad A8) is held
    run(&mut m, &[&[0x99, 46, 100]]);
    assert_eq!(m.keys_mode(), KeysMode::Auto);
    let v = run(&mut m, &[&[0x99, 43, 100], &[0x90, 60, 100], &[0x80, 60, 0], &[0x89, 43, 0]]);
    assert_eq!(v, vec![
        Out::Key { id: 7, down: true, vel: 0 },
        Out::Key { id: KEY0 + 7, down: true, vel: 100 },
        Out::Key { id: KEY0 + 7, down: false, vel: 0 },
        Out::Key { id: 7, down: false, vel: 0 },
    ]);
}

#[test]
fn a_mode_change_lets_go_of_held_notes() {
    let mut m = mpk();
    let mut v = Vec::new();
    m.set_keys_mode(KeysMode::Midi, &mut |o| v.push(o));
    run(&mut m, &[&[0x90, 60, 100]]);
    v.clear();
    m.set_keys_mode(KeysMode::Panel, &mut |o| v.push(o));
    assert_eq!(v, vec![Out::Midi { bytes: [0x80, 60, 0], len: 3 }, Out::KeysMode(KeysMode::Panel)]);
    // its release now goes nowhere
    assert!(run(&mut m, &[&[0x80, 60, 0]]).is_empty());
}

#[test]
fn learn_binds_the_next_control_and_replaces_the_old_binding() {
    let mut m = mpk();
    m.learn(Some(Target::Enc(2)));
    let v = run(&mut m, &[&[0xb3, 20, 5], &[0xb3, 20, 6]]);
    assert_eq!(v, vec![Out::Learned], "the rest of the knob's turn is not learned again");
    assert!(m.learning().is_none());
    let knob1: Vec<&Binding> = m.profile().bindings.iter().filter(|b| b.to == Target::Enc(2)).collect();
    assert_eq!(knob1.len(), 1);
    assert_eq!(knob1[0].src, Src { ch: Some(4), cc: Some(20), ..Src::default() });
    // it turns now; CC 70 is free and passes through
    let v = run(&mut m, &[&[0xb3, 20, 9], &[0xb0, 70, 1]]);
    assert_eq!(v, vec![Out::Enc { enc: 2, clicks: 3 }, Out::Midi { bytes: [0xb0, 70, 1], len: 3 }]);
}

#[test]
fn learning_a_key_zone_makes_the_note_pressed_f3() {
    let mut m = Mapper::new(Profile { name: "t".into(), ..Profile::default() });
    m.learn(Some(Target::Keys));
    run(&mut m, &[&[0x90, 36, 100]]);
    let v = run(&mut m, &[&[0x90, 38, 100]]);
    assert_eq!(v, vec![Out::Key { id: KEY0 + 2, down: true, vel: 100 }]);
    // learning panel key C4 (the 8th) from the note 48: 48 plays C4, 41 plays F3
    m.learn(Some(Target::Key(7)));
    run(&mut m, &[&[0x90, 48, 100]]);
    let v = run(&mut m, &[&[0x90, 41, 100]]);
    assert_eq!(v, vec![Out::Key { id: KEY0, down: true, vel: 100 }]);
}

#[test]
fn tags_for_the_overlay() {
    let m = mpk();
    assert_eq!(m.profile().tag_of(Target::Enc(2)).as_deref(), Some("K1"));
    assert_eq!(m.profile().tag_of(Target::Button(7)).as_deref(), Some("A8"));
    assert_eq!(m.profile().tag_of(Target::Key(7)).as_deref(), Some("60"));
}

#[test]
fn pick_by_device_name() {
    let all = builtin();
    assert_eq!(pick(&all, "MPK mini 3 MIDI 1").unwrap().name, "Akai MPK mini 3");
    assert_eq!(pick(&all, "Some Other Keyboard").unwrap().name, "Generic keyboard");
    // the names other systems give it (Android: manufacturer and product)
    for name in ["AKAI PROFESSIONAL,LP MPK mini 3", "MPK Mini Mk3", "MPKmini3", "mpk-mini-3 MIDI 1"] {
        assert_eq!(pick_named(&all, name).map(|p| p.name.as_str()), Some("Akai MPK mini 3"), "{name}");
    }
    // nothing made for it: none (fm1-live's auto then passes its MIDI through)
    assert!(pick_named(&all, "Some Other Keyboard").is_none());
    assert!(pick_named(&all, "loopMIDI Port").is_none());
    assert_eq!(Profile { name: "My Pads / 2".into(), ..Profile::default() }.file_name(), "my-pads---2.json");
}

#[test]
fn releases_get_through_while_learning() {
    let mut m = mpk();
    // a note held on the panel's keys and pad A8 (GLO) held, then learning starts
    let v = run(&mut m, &[&[0x90, 60, 100], &[0x99, 43, 100]]);
    assert_eq!(v.len(), 2, "{v:?}");
    m.learn(Some(Target::Enc(2)));
    let v = run(&mut m, &[&[0x80, 60, 0], &[0x89, 43, 0]]);
    assert_eq!(v, vec![Out::Key { id: KEY0 + 7, down: false, vel: 0 }, Out::Key { id: 7, down: false, vel: 0 }],
        "the note and the pad let go");
    assert_eq!(m.learning(), Some(Target::Enc(2)), "a release learns nothing");
    // and the note plays again after learning
    m.learn(None);
    let v = run(&mut m, &[&[0x90, 60, 100]]);
    assert_eq!(v, vec![Out::Key { id: KEY0 + 7, down: true, vel: 100 }]);
}

#[test]
fn only_what_works_is_learned() {
    let mut m = mpk();
    m.learn(Some(Target::Key(3)));
    // pitch bend and a CC onto the keys: nothing learned, still learning
    assert!(run(&mut m, &[&[0xe0, 0, 80], &[0xb0, 1, 64]]).is_empty());
    assert_eq!(m.learning(), Some(Target::Key(3)));
    assert_eq!(run(&mut m, &[&[0x90, 50, 100]]), vec![Out::Learned]);
    // pitch bend onto an encoder: nothing learned
    m.learn(Some(Target::Enc(2)));
    assert!(run(&mut m, &[&[0xe0, 0, 80]]).is_empty());
    assert_eq!(m.learning(), Some(Target::Enc(2)));
}

#[test]
fn system_common_messages_pass_through_a_profile() {
    let mut m = mpk();
    let v = run(&mut m, &[&[0xf2, 0x10, 0x02], &[0xf3, 5], &[0xf1, 0x23], &[0xf6], &[0xf8]]);
    assert_eq!(v, vec![
        Out::Midi { bytes: [0xf2, 0x10, 0x02], len: 3 },
        Out::Midi { bytes: [0xf3, 5, 0], len: 2 },
        Out::Midi { bytes: [0xf1, 0x23, 0], len: 2 },
        Out::Midi { bytes: [0xf6, 0, 0], len: 1 },
        Out::Midi { bytes: [0xf8, 0, 0], len: 1 },
    ]);
}

#[test]
fn notes_beyond_a_zone_that_does_not_fold_go_to_midi_in() {
    let mut p = Profile { name: "t".into(), ..Profile::default() };
    let mut b = Binding::new(Src { notes: Some([0, 127]), ..Src::default() }, Target::Keys);
    b.fold = false;
    p.bindings.push(b);
    let mut m = Mapper::new(p);
    let v = run(&mut m, &[&[0x90, 30, 90], &[0x80, 30, 0], &[0x90, 53, 90]]);
    assert_eq!(v, vec![
        Out::Midi { bytes: [0x90, 30, 90], len: 3 },
        Out::Midi { bytes: [0x80, 30, 0], len: 3 },
        Out::Key { id: KEY0, down: true, vel: 90 },
    ]);
}

#[test]
fn learning_into_a_built_in_makes_your_own_copy() {
    let mut m = Mapper::new(mpk_profile());
    m.learn(Some(Target::Enc(2)));
    run(&mut m, &[&[0xb3, 20, 5]]);
    assert_eq!(m.profile().name, "Akai MPK mini 3 (mine)");
    // your copy is the one an MPK picks; the built-in is still there
    let mut all = builtin();
    all.push(m.profile().clone());
    assert_eq!(pick_named(&all, "MPK mini 3").map(|p| p.name.as_str()), Some("Akai MPK mini 3 (mine)"));
    assert!(all.iter().any(|p| p.name == "Akai MPK mini 3"));
    // a profile of your own keeps its name
    let mut m = Mapper::new(Profile { name: "Mine".into(), ..Profile::default() });
    m.learn(Some(Target::Enc(2)));
    run(&mut m, &[&[0xb0, 21, 5]]);
    assert_eq!(m.profile().name, "Mine");
}

#[test]
fn program_changes_and_aftertouch_are_not_learned() {
    let mut m = mpk();
    m.learn(Some(Target::Midi));
    assert!(run(&mut m, &[&[0xc0, 5], &[0xd0, 40], &[0xa0, 60, 40]]).is_empty());
    assert_eq!(m.learning(), Some(Target::Midi));
    assert!(!m.profile().bindings.iter().any(|b| b.src.bend));
}

/// A profile with CC `cc` on button `id` in `mode`.
fn cc_button(cc: u8, id: u8, mode: Mode) -> Binding {
    let mut b = Binding::new(Src { cc: Some(cc), ..Src::default() }, Target::Button(id));
    b.mode = mode;
    b
}

#[test]
fn a_cc_holds_a_button_from_64_up_until_it_goes_below() {
    // a pedal or a fader: many values on the way down and back, one press and one release
    let mut m = Mapper::new(Profile { name: "t".into(), bindings: vec![cc_button(64, 12, Mode::Momentary)], ..Profile::default() });
    let v = run(&mut m, &[&[0xb0, 64, 20], &[0xb0, 64, 70], &[0xb0, 64, 100], &[0xb0, 64, 127], &[0xb0, 64, 90], &[0xb0, 64, 10], &[0xb0, 64, 0]]);
    assert_eq!(v, vec![Out::Key { id: 12, down: true, vel: 0 }, Out::Key { id: 12, down: false, vel: 0 }]);
    // and however many values it sends while down (a pad's pressure), it lets go at the first one below
    let mut v = Vec::new();
    for i in 0..600 {
        m.feed(&[0xb0, 64, 64 + (i % 64) as u8], &mut |o| v.push(o));
    }
    m.feed(&[0xb0, 64, 63], &mut |o| v.push(o));
    assert_eq!(v, vec![Out::Key { id: 12, down: true, vel: 0 }, Out::Key { id: 12, down: false, vel: 0 }]);
}

#[test]
fn a_cc_toggles_a_button_once_a_press() {
    let mut m = Mapper::new(Profile { name: "t".into(), bindings: vec![cc_button(20, 10, Mode::Toggle)], ..Profile::default() });
    let v = run(&mut m, &[&[0xb0, 20, 100], &[0xb0, 20, 127], &[0xb0, 20, 80], &[0xb0, 20, 0]]);
    assert_eq!(v, vec![Out::Key { id: 10, down: true, vel: 0 }], "one press latches it, whatever it sends while down");
    let v = run(&mut m, &[&[0xb0, 20, 127], &[0xb0, 20, 110], &[0xb0, 20, 0]]);
    assert_eq!(v, vec![Out::Key { id: 10, down: false, vel: 0 }], "the next press lets it go");
}

#[test]
fn a_pad_and_a_cc_hold_one_button_until_both_let_go() {
    let mut p = mpk_profile();
    p.bindings.push(cc_button(64, 7, Mode::Momentary)); // (pad A8, note 43 on channel 10, is GLO: 7)
    let mut m = Mapper::new(p);
    let v = run(&mut m, &[&[0x99, 43, 100], &[0xb0, 64, 100], &[0xb0, 64, 110], &[0x89, 43, 0]]);
    assert_eq!(v, vec![Out::Key { id: 7, down: true, vel: 0 }], "the CC still holds it");
    let v = run(&mut m, &[&[0xb0, 64, 0]]);
    assert_eq!(v, vec![Out::Key { id: 7, down: false, vel: 0 }]);
    // the other way round: the CC lets go first
    let v = run(&mut m, &[&[0xb0, 64, 127], &[0x99, 43, 100], &[0xb0, 64, 0], &[0x89, 43, 0]]);
    assert_eq!(v, vec![Out::Key { id: 7, down: true, vel: 0 }, Out::Key { id: 7, down: false, vel: 0 }]);
}

#[test]
fn a_held_cc_button_lets_go_after_something_else_was_learned() {
    let mut m = Mapper::new(Profile { name: "t".into(), bindings: vec![cc_button(64, 12, Mode::Momentary)], ..Profile::default() });
    assert_eq!(run(&mut m, &[&[0xb0, 64, 127]]).len(), 1);
    // learning puts its binding first: the held one keeps its place in what the mapper holds
    m.learn(Some(Target::Enc(2)));
    assert_eq!(run(&mut m, &[&[0xb0, 21, 5]]), vec![Out::Learned]);
    assert_eq!(run(&mut m, &[&[0xb0, 64, 0]]), vec![Out::Key { id: 12, down: false, vel: 0 }]);
}

#[test]
fn a_cc_presses_once_as_it_comes_up_to_64() {
    // what a press does once (the recorder, the labels, the keys' mode, a step of an encoder): a pedal,
    // a fader or a pad's pressure sends many values at 64 or more, and they are one press
    let mut step = Binding::new(Src { cc: Some(23), ..Src::default() }, Target::Enc(2));
    step.mode = Mode::Step;
    let bindings = vec![
        Binding::new(Src { cc: Some(20), ..Src::default() }, Target::Record),
        Binding::new(Src { cc: Some(21), ..Src::default() }, Target::Labels),
        Binding::new(Src { cc: Some(22), ..Src::default() }, Target::KeysMode),
        step,
        Binding::new(Src { note: Some(40), ..Src::default() }, Target::Record),
    ];
    let mut m = Mapper::new(Profile { name: "t".into(), bindings, ..Profile::default() });
    for (cc, once) in [(20, Out::Record), (21, Out::Labels), (23, Out::Enc { enc: 2, clicks: 1 })] {
        let v = run(&mut m, &[&[0xb0, cc, 30], &[0xb0, cc, 64], &[0xb0, cc, 90], &[0xb0, cc, 127], &[0xb0, cc, 100]]);
        assert_eq!(v, vec![once], "CC {cc}: one press");
        let v = run(&mut m, &[&[0xb0, cc, 63], &[0xb0, cc, 10], &[0xb0, cc, 127], &[0xb0, cc, 120]]);
        assert_eq!(v, vec![once], "CC {cc}: let go, and pressed again");
    }
    let v = run(&mut m, &[&[0xb0, 22, 100], &[0xb0, 22, 127], &[0xb0, 22, 0], &[0xb0, 22, 127]]);
    assert_eq!(v, vec![Out::KeysMode(KeysMode::Midi), Out::KeysMode(KeysMode::Auto)], "the keys' mode moves on once a press");
    // a note's on and off are a pair as they come: each on is a press
    let v = run(&mut m, &[&[0x90, 40, 100], &[0x80, 40, 0], &[0x90, 40, 100], &[0x80, 40, 0]]);
    assert_eq!(v, vec![Out::Record, Out::Record]);
}

#[test]
fn a_control_that_sends_no_release_presses_each_time_it_sends_its_value() {
    // a trigger-only button sends 127 a press and nothing when let go. A pedal or a fader as a rule
    // sends a value only when it changed, so the same value twice in a row is taken for that button
    // pressed again (a guess: `Mapper::pressed`)
    let bindings = vec![
        Binding::new(Src { cc: Some(20), ..Src::default() }, Target::Record),
        cc_button(21, 10, Mode::Toggle),
        cc_button(22, 12, Mode::Momentary),
    ];
    let mut m = Mapper::new(Profile { name: "t".into(), bindings, ..Profile::default() });
    let (on, off) = (Out::Key { id: 10, down: true, vel: 0 }, Out::Key { id: 10, down: false, vel: 0 });

    assert_eq!(run(&mut m, &[&[0xb0, 20, 127], &[0xb0, 20, 127], &[0xb0, 20, 127]]), vec![Out::Record; 3]);
    assert_eq!(run(&mut m, &[&[0xb0, 21, 127], &[0xb0, 21, 127], &[0xb0, 21, 127]]), vec![on, off, on], "three presses, three toggles");

    // a fader or a pedal on its way up: one press (from below 64: the mapper starts there again)
    let mut m = Mapper::new(m.profile().clone());
    assert_eq!(run(&mut m, &[&[0xb0, 20, 64], &[0xb0, 20, 80], &[0xb0, 20, 100], &[0xb0, 20, 127]]), vec![Out::Record]);
    assert_eq!(run(&mut m, &[&[0xb0, 21, 64], &[0xb0, 21, 80], &[0xb0, 21, 100], &[0xb0, 21, 127]]), vec![on]);
    // a button that sends its release: a press each time it comes up
    let mut m = Mapper::new(m.profile().clone());
    assert_eq!(run(&mut m, &[&[0xb0, 20, 127], &[0xb0, 20, 0], &[0xb0, 20, 127]]), vec![Out::Record; 2]);
    assert_eq!(run(&mut m, &[&[0xb0, 21, 127], &[0xb0, 21, 0], &[0xb0, 21, 127]]), vec![on, off]);

    // a button that is held while its control is: with no release it stays held, once, and the one
    // value below 64 lets it go
    let v = run(&mut m, &[&[0xb0, 22, 127], &[0xb0, 22, 127], &[0xb0, 22, 0]]);
    assert_eq!(v, vec![Out::Key { id: 12, down: true, vel: 0 }, Out::Key { id: 12, down: false, vel: 0 }]);
}

#[test]
fn a_learned_cc_starts_at_the_value_it_was_learned_with() {
    let mut m = Mapper::new(Profile { name: "t".into(), ..Profile::default() });
    // learned from a fader on its way up: the rest of its way is not a press; down and up again is
    m.learn(Some(Target::Record));
    assert_eq!(run(&mut m, &[&[0xb0, 20, 70]]), vec![Out::Learned]);
    assert_eq!(run(&mut m, &[&[0xb0, 20, 71], &[0xb0, 20, 90], &[0xb0, 20, 127]]), vec![]);
    assert_eq!(run(&mut m, &[&[0xb0, 20, 10], &[0xb0, 20, 100]]), vec![Out::Record]);
    // a control that sends no release, learned by its press: each press after it is one
    m.learn(Some(Target::Labels));
    assert_eq!(run(&mut m, &[&[0xb0, 21, 127]]), vec![Out::Learned]);
    assert_eq!(run(&mut m, &[&[0xb0, 21, 127], &[0xb0, 21, 127]]), vec![Out::Labels, Out::Labels]);
    // a pad learned by its press and still down when something else is to be learned: its release
    // is the release of that press, not what the other target learns
    m.learn(Some(Target::Button(3)));
    assert_eq!(run(&mut m, &[&[0xb0, 22, 127]]), vec![Out::Learned]);
    m.learn(Some(Target::KeysMode));
    assert_eq!(run(&mut m, &[&[0xb0, 22, 0]]), vec![]);
    assert_eq!(m.learning(), Some(Target::KeysMode));
    // and the pad's next press holds its button, from the press to the release
    m.learn(None);
    let v = run(&mut m, &[&[0xb0, 22, 127], &[0xb0, 22, 0]]);
    assert_eq!(v, vec![Out::Key { id: 3, down: true, vel: 0 }, Out::Key { id: 3, down: false, vel: 0 }]);
}
