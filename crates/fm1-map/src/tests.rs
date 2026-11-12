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
    // as shipped: auto, MIDI IN on channel 16 (SLOOP: the selected track)
    let mut m = Mapper::new(mpk_profile());
    assert_eq!(m.keys_mode(), KeysMode::Auto);
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
    assert_eq!(Profile { name: "My Pads / 2".into(), ..Profile::default() }.file_name(), "my-pads---2.json");
}
