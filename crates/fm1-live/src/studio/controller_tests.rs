use fm1_map::{Mapper, Out, Profile};
fn mapper() -> Mapper {
    Mapper::new(Profile::from_json(include_str!("minilab.json")).unwrap())
}
fn feed(m: &mut Mapper, msg: &[u8]) -> Vec<Out> {
    let mut out = Vec::new();
    m.feed(msg, &mut |o| out.push(o));
    out
}
#[test]
fn relative_knobs_repeat_and_command_pads_do_not_reach_firmware_midi() {
    let mut m = mapper();
    for (cc, enc) in [
        (71, 0),
        (76, 6),
        (77, 1),
        (93, 2),
        (18, 3),
        (19, 4),
        (16, 5),
        (114, 0),
        (112, 6),
    ] {
        for (value, clicks) in [(65, 1), (65, 1), (63, -1), (67, 3)] {
            assert!(
                matches!(feed(&mut m,&[0xb0,cc,value]).as_slice(),[Out::Enc{enc:e,clicks:c}] if *e==enc && *c==clicks)
            );
        }
        assert!(feed(&mut m, &[0xb0, cc, 64]).is_empty());
    }
    for (cc, id) in (20..36).zip([8, 2, 6, 11, 3, 12, 13, 7, 4, 5, 10, 9, 0, 1, 8, 3]) {
        assert!(
            matches!(feed(&mut m,&[0xbf,cc,127]).as_slice(),[Out::Key{id:i,down:true,..}] if *i==id)
        );
        assert!(feed(&mut m, &[0xbf, cc, 127]).is_empty());
        assert!(
            matches!(feed(&mut m,&[0xbf,cc,0]).as_slice(),[Out::Key{id:i,down:false,..}] if *i==id)
        );
    }
}
#[test]
fn duplicate_holds_release_only_after_every_source() {
    let mut m = mapper();
    feed(&mut m, &[0xbf, 20, 127]);
    assert!(feed(&mut m, &[0xbf, 34, 127]).is_empty());
    assert!(feed(&mut m, &[0xbf, 20, 0]).is_empty());
    assert!(matches!(
        feed(&mut m, &[0xbf, 34, 0]).as_slice(),
        [Out::Key {
            id: 8,
            down: false,
            ..
        }]
    ));
}
#[test]
fn pedals_and_unmapped_faders_pass_through() {
    let mut m = mapper();
    for msg in [
        [0xb0, 64, 127],
        [0xb0, 82, 100],
        [0xb1, 71, 65],
        [0xb0, 21, 127],
    ] {
        assert!(matches!(feed(&mut m,&msg).as_slice(),[Out::Midi{bytes,len:3}] if *bytes==msg));
    }
    for cc in [9, 113] {
        assert!(feed(&mut m, &[0xb0, cc, 127]).is_empty());
    }
    for (v, want) in [(0, 0), (127, 1023)] {
        assert!(
            matches!(feed(&mut m,&[0xb0,74,v]).as_slice(),[Out::Adc{ch:4,value}] if *value==want)
        );
    }
}
#[test]
fn panic_releases_profile_buttons_before_forwarding_on_remapped_channel() {
    let control = crate::ctl::Ctl::new(
        Some(Profile::from_json(include_str!("minilab.json")).unwrap()),
        2,
        true,
    );
    let (tx, rx) = std::sync::mpsc::channel();
    control.feed("Minilab3 MIDI", &[0xbf, 21, 127], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 2, down: true }
    ));
    control.feed("Minilab3 MIDI", &[0xb0, 123, 0], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 2, down: false }
    ));
    for expected in [0xb1, 123, 0] {
        assert!(matches!(rx.recv().unwrap(),fm1_soc::Action::Midi{b} if b==expected));
    }
    assert!(!control.view.lock().unwrap().down[2]);
}

fn factory() -> Profile {
    Profile::from_json(include_str!("minilab-factory.json")).unwrap()
}
#[test]
fn finder_defaults_select_factory_and_respond_to_captured_knob_and_pad_bytes() {
    let profiles = crate::ctl::bundled_profiles();
    let profile =
        crate::ctl::choose_from("auto", &["M4".into(), "Minilab3 MIDI".into()], &profiles)
            .unwrap()
            .unwrap();
    assert_eq!(profile.name, crate::ctl::MINILAB_FACTORY);
    let control = crate::ctl::Ctl::new(Some(profile), 0, true);
    let (tx, rx) = std::sync::mpsc::channel();
    // Captured from the connected controller: absolute CC74, then channel-10 pad 36.
    assert!(control.feed("Minilab3 MIDI", &[0xb0, 0x4a, 0x3b], &tx));
    assert!(rx.try_recv().is_err());
    control.feed("Minilab3 MIDI", &[0xb0, 0x4a, 0x3c], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Enc { enc: 2, clicks: 1 }
    ));
    control.feed("Minilab3 MIDI", &[0x99, 0x24, 0x0d], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 8, down: true }
    ));
    control.feed("Minilab3 MIDI", &[0x89, 0x24, 0], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 8, down: false }
    ));
}
#[test]
fn automatic_profile_survives_startup_without_controller_and_ignores_other_ports() {
    let profiles = crate::ctl::bundled_profiles();
    let profile = crate::ctl::choose_from("auto", &[], &profiles)
        .unwrap()
        .unwrap();
    let control = crate::ctl::Ctl::new(Some(profile), 0, true);
    for name in ["Minilab3 MIDI", "MiniLab 3 MIDI"] {
        assert!(control.maps(name));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    for name in [
        "Minilab3 MCU/HUI",
        "Minilab3 ALV",
        "Minilab3 DIN THRU",
        "M4",
    ] {
        assert!(!control.maps(name));
        assert!(!control.feed(name, &[0x99, 36, 100], &tx));
    }
    assert!(rx.try_recv().is_err());
    assert!(control.feed("Minilab3 MIDI", &[0x99, 36, 100], &tx));
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 8, down: true }
    ));
}
#[test]
fn live_profile_switch_releases_holds_and_changes_existing_input_routing() {
    let control = crate::ctl::Ctl::new(Some(factory()), 0, true);
    let (tx, rx) = std::sync::mpsc::channel();
    control.feed("Minilab3 MIDI", &[0x99, 36, 100], &tx);
    let _ = rx.recv().unwrap();
    control.configure(None, false, &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 8, down: false }
    ));
    assert!(!control.enabled());
    assert!(!control.feed("Minilab3 MIDI", &[0xb0, 71, 65], &tx));
    let user = Profile::from_json(include_str!("minilab.json")).unwrap();
    control.configure(Some(user), false, &tx);
    assert!(control.enabled());
    for _ in 0..2 {
        assert!(control.feed("Minilab3 MIDI", &[0xb0, 71, 65], &tx));
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::Enc { enc: 0, clicks: 1 }
        ));
    }
    assert!(!control.feed("Minilab3 MCU/HUI", &[0xb0, 71, 65], &tx));
}
#[test]
fn both_factory_pad_banks_use_panel_commands_and_release_shared_buttons() {
    let mut m = Mapper::new(factory());
    for (note, id) in (36..52).zip([8, 2, 6, 11, 3, 12, 13, 7, 4, 5, 10, 9, 0, 1, 8, 3]) {
        assert!(
            matches!(feed(&mut m,&[0x99,note,100]).as_slice(),[Out::Key{id:i,down:true,..}] if *i==id)
        );
        assert!(
            matches!(feed(&mut m,&[0x89,note,0]).as_slice(),[Out::Key{id:i,down:false,..}] if *i==id)
        );
    }
    feed(&mut m, &[0x99, 36, 100]);
    assert!(feed(&mut m, &[0x99, 50, 100]).is_empty());
    assert!(feed(&mut m, &[0x89, 36, 0]).is_empty());
    assert!(matches!(
        feed(&mut m, &[0x89, 50, 0]).as_slice(),
        [Out::Key {
            id: 8,
            down: false,
            ..
        }]
    ));
}
#[test]
fn changing_profile_keeps_firmware_memory_and_input_settings_in_launch_arguments() {
    let args: Vec<String> = [
        "--fwsc",
        "instrument.fwsc",
        "--flash",
        "saved.bin",
        "--map",
        "none",
        "--midi",
        "Minilab3 MIDI",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let changed = crate::ctl::controller_args(&args, crate::ctl::MINILAB_USER);
    let expected: Vec<String> = [
        "--fwsc",
        "instrument.fwsc",
        "--flash",
        "saved.bin",
        "--midi",
        "Minilab3 MIDI",
        "--map",
        crate::ctl::MINILAB_USER,
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(changed, expected);
    assert_eq!(
        crate::ctl::controller_args(&changed, crate::ctl::MINILAB_USER),
        changed
    );
}

#[test]
fn factory_knobs_put_master_before_navigation() {
    let mut m = Mapper::new(factory());
    for (cc, enc) in [
        (74, 2),
        (71, 3),
        (76, 4),
        (77, 5),
        (18, 0),
        (19, 6),
        (16, 1),
    ] {
        assert!(feed(&mut m, &[0xb0, cc, 64]).is_empty());
        assert!(
            matches!(feed(&mut m,&[0xb0,cc,65]).as_slice(),[Out::Enc{enc:e,clicks:1}] if *e==enc)
        );
    }
    assert!(matches!(
        feed(&mut m, &[0xb0, 93, 127]).as_slice(),
        [Out::Adc { ch: 4, value: 1023 }]
    ));
}

#[test]
fn minilab_keyboard_uses_panel_keys_with_velocity_and_release() {
    for profile in crate::ctl::bundled_profiles() {
        let control = crate::ctl::Ctl::new(Some(profile), 0, true);
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(control.feed("Minilab3 MIDI", &[0x90, 60, 87], &tx));
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::KeyVel { vel: 87 }
        ));
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::Key { id: 21, down: true }
        ));
        assert!(control.view.lock().unwrap().down[21]);
        assert!(rx.try_recv().is_err());
        control.feed("Minilab3 MIDI", &[0x80, 60, 0], &tx);
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::Key {
                id: 21,
                down: false
            }
        ));
        assert!(!control.view.lock().unwrap().down[21]);
        assert!(rx.try_recv().is_err());
        control.feed("Minilab3 MIDI", &[0x90, 60, 100], &tx);
        while rx.try_recv().is_ok() {}
        control.feed("Minilab3 MIDI", &[0x90, 60, 0], &tx);
        assert!(matches!(
            rx.recv().unwrap(),
            fm1_soc::Action::Key {
                id: 21,
                down: false
            }
        ));
        assert!(rx.try_recv().is_err());
    }
}
