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
fn notes_pedals_and_unmapped_faders_pass_through() {
    let mut m = mapper();
    for msg in [
        [0x90, 60, 100],
        [0x80, 60, 0],
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
    );
    let (tx, rx) = std::sync::mpsc::channel();
    control.feed(&[0xbf, 21, 127], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 2, down: true }
    ));
    control.feed(&[0xb0, 123, 0], &tx);
    assert!(matches!(
        rx.recv().unwrap(),
        fm1_soc::Action::Key { id: 2, down: false }
    ));
    for expected in [0xb1, 123, 0] {
        assert!(matches!(rx.recv().unwrap(),fm1_soc::Action::Midi{b} if b==expected));
    }
    assert!(!control.view.lock().unwrap().down[2]);
}
