//! The plugin's window: the front panel (the native window's and the page's drawing code) with the
//! machine's screen and LEDs, played with the mouse as on the page (keys and buttons held while
//! pressed, a drag across the keys a glissando, knobs dragged up / down or scrolled, MASTER a pot
//! that is the host's "Master" parameter), and a bar to load a firmware package.

use crate::engine::Cmd;
use crate::{panel, panel_smk, Fm1Params, Link};
use fm1_soc::Board;
use nih_plug::prelude::*;
use nih_plug_egui::{create_egui_editor, egui, widgets};
use std::sync::Arc;

/// A pointer's grab: what it hit (fm1-web's panel_hit codes), where it was, what it holds.
struct Grab {
    ctl: i32,
    last_y: f32,
    acc: f32,
    held: i32,
}

struct Ui {
    board: Board,
    base: Vec<u32>,
    buf: Vec<u32>,
    rgba: Vec<u8>,
    tex: Option<egui::TextureHandle>,
    down: [bool; 64],
    enc_clicks: [i32; 8],
    turning: [u32; 9],
    /// SMK-37: its analog inputs as set (fm1_soc::smk's ANALOG_* indices)
    analog: [u16; 23],
    grab: Option<Grab>,
    wheel_acc: f32,
    msg: String,
    msg_until: Option<std::time::Instant>,
    /// the file dialog's answer, while it is open
    picking: Option<(FileAction, Answer)>,
    job: Option<Job>,
}

impl Default for Ui {
    fn default() -> Self {
        let mut analog = [0; 23];
        analog[fm1_soc::smk::ANALOG_PITCH as usize] = fm1_soc::smk::PITCH_REST;
        analog[fm1_soc::smk::ANALOG_MOD as usize] = fm1_soc::smk::MOD_REST;
        Ui {
            board: Board::Fm1,
            base: Vec::new(),
            buf: Vec::new(),
            rgba: Vec::new(),
            tex: None,
            down: [false; 64],
            enc_clicks: [0; 8],
            turning: [0; 9],
            analog,
            grab: None,
            wheel_acc: 0.0,
            msg: String::new(),
            msg_until: None,
            picking: None,
            job: None,
        }
    }
}

const NKEY: i32 = 37;

pub fn create(params: Arc<Fm1Params>, link: Arc<Link>) -> Option<Box<dyn Editor>> {
    // (the state outlives a window: one opened again has a new egui context, so the panel's
    // texture, which belongs to the old one, is made again)
    let build = |_: &egui::Context, ui: &mut Ui| {
        ui.tex = None;
        ui.grab = None;
    };
    create_egui_editor(params.editor_state.clone(), Ui::default(), build, move |ctx, setter, ui| {
        let (lcd, leds, pads, board, booted, halted, speed, late) = {
            let v = link.shared.view.lock().unwrap();
            (v.lcd.clone(), v.leds, v.pads, v.board, v.booted, v.halted, v.speed, v.late)
        };
        if board != ui.board || ui.base.is_empty() {
            ui.board = board;
            ui.base = if board == Board::Smk37 { panel_smk::base() } else { panel::base() };
            ui.buf = vec![0; ui.base.len()];
        }
        let smk = board == Board::Smk37;
        let (w, h) = if smk { (panel_smk::W, panel_smk::H) } else { (panel::W, panel::H) };

        egui::TopBottomPanel::top("bar").show(ctx, |bar| {
            bar.horizontal_wrapped(|bar| {
                if ui.msg_until.is_some_and(|until| std::time::Instant::now() >= until) {
                    ui.msg.clear(); ui.msg_until = None;
                }
                if let Some(result) = ui.job.as_ref().and_then(|job| job.lock().unwrap().take()) {
                    ui.job = None;
                    ui.msg_until = result.as_ref().ok().map(|_| std::time::Instant::now() + std::time::Duration::from_secs(3));
                    ui.msg = match result { Ok(message) => message, Err(error) => format!("Save file: {error}") };
                }
                let name = params.firmware.lock().unwrap().name.clone();
                let idle = ui.picking.is_none() && ui.job.is_none();
                if bar.add_enabled(idle, egui::Button::new("Load firmware / save…"))
                    .on_hover_text("Open a .fwsc firmware package or a shared .fm1save setup").clicked() {
                    ui.picking = Some((FileAction::Load, pick(FileAction::Load, &name)));
                }
                let has_firmware = !params.firmware.lock().unwrap().pkg.is_empty();
                if bar.add_enabled(idle && has_firmware, egui::Button::new("Export save…"))
                    .on_hover_text("Share the running firmware, patches and settings as a .fm1save file").clicked() {
                    ui.picking = Some((FileAction::Export, pick(FileAction::Export, &name)));
                }
                let chosen = ui.picking.as_ref().and_then(|(action, answer)|
                    answer.lock().unwrap().take().map(|path| (*action, path)));
                if let Some((action, path)) = chosen {
                    ui.picking = None;
                    if let Some(path) = path {
                        ui.msg_until = None;
                        ui.msg = if matches!(action, FileAction::Load) { "Loading save…" } else { "Exporting live flash…" }.into();
                        ui.job = Some(file_job(action, path, params.clone(), link.clone()));
                    }
                }
                bar.label(egui::RichText::new(if name.is_empty() { "no firmware".into() } else { name }).strong());
                bar.separator();
                bar.label("MIDI channel");
                bar.add(widgets::ParamSlider::for_param(&params.channel, setter).with_width(90.0));
                bar.label("CPU clock").on_hover_text("The emulated FM-1's clock, 240 MHz as on the chip. Lower it if this computer can't keep up with a heavy firmware (\"frames late\"); raise it if a firmware runs out of time. Changing it restarts the firmware");
                bar.add(widgets::ParamSlider::for_param(&params.clock, setter).with_width(90.0));
                bar.separator();
                let status = if !ui.msg.is_empty() {
                    ui.msg.clone()
                } else if !booted {
                    "Open a .fwsc firmware or shared .fm1save to start".into()
                } else if halted {
                    "The firmware stopped: an instruction the emulator does not know".into()
                } else {
                    let late = if late > 0 { format!(", {late} frames late in the last second") } else { String::new() };
                    if speed > 0.0 { format!("could run {speed:.1}x real time{late}") } else { format!("starting{late}") }
                };
                bar.label(status);
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(0x18, 0x19, 0x1b))).show(ctx, |c| {
            let avail = c.available_rect_before_wrap();
            let k = (avail.width() / w as f32).min(avail.height() / h as f32);
            let rect = egui::Rect::from_center_size(avail.center(), egui::vec2(w as f32 * k, h as f32 * k));
            let resp = c.allocate_rect(rect, egui::Sense::click_and_drag());
            let at = |p: egui::Pos2| ((p.x - rect.min.x) / k, (p.y - rect.min.y) / k);

            // the pointer
            let (pressed, released, pos, scroll) =
                ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.interact_pos(), i.raw_scroll_delta.y));
            if pressed && resp.hovered() {
                if let Some(p) = pos {
                    let (x, y) = at(p);
                    let ctl = hit(smk, x, y);
                    let mut g = Grab { ctl, last_y: y, acc: 0.0, held: -1 };
                    if (100..200).contains(&ctl) {
                        g.held = ctl - 100;
                        if smk && g.held < NKEY {
                            velocity(&link, panel_smk_hit_y(x, y));
                        }
                        key(ui, &link, g.held, true);
                    } else if smk && ctl >= 200 {
                        slide(ui, &link, ctl, y);
                    } else if !smk && ctl == 7 {
                        setter.begin_set_parameter(&params.master);
                    }
                    ui.grab = Some(g);
                }
            }
            if let (Some(g), Some(p)) = (ui.grab.as_mut(), pos) {
                let (x, y) = at(p);
                let keys = if smk { 0..NKEY } else { 14..41 };
                if (100..200).contains(&g.ctl) && keys.contains(&g.held) {
                    let h = hit(smk, x, y);
                    if keys.contains(&(h - 100)) && h - 100 != g.held {
                        let old = g.held;
                        g.held = h - 100;
                        let new = g.held;
                        key(ui, &link, old, false);
                        if smk {
                            velocity(&link, panel_smk_hit_y(x, y));
                        }
                        key(ui, &link, new, true);
                    }
                } else if smk && g.ctl >= 300 {
                    let ctl = g.ctl;
                    slide(ui, &link, ctl, y);
                } else if g.ctl >= 0 && g.ctl < if smk { 8 } else { 7 } {
                    // 12 px of drag a click
                    g.acc += g.last_y - y;
                    let clicks = (g.acc / 12.0).trunc();
                    g.acc -= clicks * 12.0;
                    let ctl = g.ctl;
                    if clicks != 0.0 {
                        turn(ui, &link, smk, ctl, clicks as i32);
                    }
                } else if !smk && g.ctl == 7 {
                    let v = (params.master.value() + (g.last_y - y) * 4.0 / 1023.0).clamp(0.0, 1.0);
                    setter.set_parameter(&params.master, v);
                    ui.turning[7] = 10;
                }
                if let Some(g) = ui.grab.as_mut() {
                    g.last_y = y;
                }
            }
            if released {
                if let Some(g) = ui.grab.take() {
                    if g.held >= 0 {
                        key(ui, &link, g.held, false);
                    }
                    if smk && (200..216).contains(&g.ctl) {
                        analog(ui, &link, (g.ctl - 200) as usize, 0); // a pad let go
                    }
                    if smk && g.ctl == 400 {
                        analog(ui, &link, fm1_soc::smk::ANALOG_PITCH as usize, fm1_soc::smk::PITCH_REST); // springs back
                    }
                    if !smk && g.ctl == 7 {
                        setter.end_set_parameter(&params.master);
                    }
                }
            }
            if scroll != 0.0 && resp.hovered() {
                if let Some(p) = pos {
                    let (x, y) = at(p);
                    let h = hit(smk, x, y);
                    if smk && (300..304).contains(&h) {
                        let i = 16 + (h - 300) as usize;
                        analog(ui, &link, i, (ui.analog[i] as f32 + scroll * 2.0).clamp(0.0, 1023.0) as u16);
                    } else if !smk && h == 7 {
                        let v = (params.master.value() + scroll * 0.5 / 1023.0).clamp(0.0, 1.0);
                        setter.begin_set_parameter(&params.master);
                        setter.set_parameter(&params.master, v);
                        setter.end_set_parameter(&params.master);
                    } else if (0..if smk { 8 } else { 7 }).contains(&h) {
                        ui.wheel_acc += scroll;
                        while ui.wheel_acc >= 40.0 {
                            ui.wheel_acc -= 40.0;
                            turn(ui, &link, smk, h, 1);
                        }
                        while ui.wheel_acc <= -40.0 {
                            ui.wheel_acc += 40.0;
                            turn(ui, &link, smk, h, -1);
                        }
                    }
                }
            }

            // the panel
            if smk {
                let a = &ui.analog;
                let st = panel_smk::State {
                    lcd: &lcd,
                    enc_clicks: &ui.enc_clicks,
                    turning: &std::array::from_fn(|i| ui.turning[i]),
                    down: &ui.down,
                    leds: &std::array::from_fn(|i| leds[i]),
                    pads: &pads,
                    pad_force: &std::array::from_fn(|p| a[p]),
                    faders: &std::array::from_fn(|f| a[16 + f]),
                    pitch: a[fm1_soc::smk::ANALOG_PITCH as usize],
                    modw: a[fm1_soc::smk::ANALOG_MOD as usize],
                };
                panel_smk::draw(&mut ui.buf, &ui.base, &st);
            } else {
                let st = panel::State {
                    lcd: &lcd,
                    enc_clicks: &std::array::from_fn(|i| ui.enc_clicks[i]),
                    pot: (params.master.value() * 1023.0).round() as u16,
                    down: &std::array::from_fn(|i| ui.down[i]),
                    leds: &leds,
                    turning: &std::array::from_fn(|i| ui.turning[i]),
                };
                panel::draw(&mut ui.buf, &ui.base, &st);
            }
            for t in ui.turning.iter_mut() {
                *t = t.saturating_sub(1);
            }
            ui.rgba.clear();
            ui.rgba.extend(ui.buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]));
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &ui.rgba);
            match ui.tex.as_mut() {
                Some(t) if t.size() == [w, h] => t.set(img, egui::TextureOptions::LINEAR),
                _ => ui.tex = Some(ctx.load_texture("panel", img, egui::TextureOptions::LINEAR)),
            }
            if let Some(t) = &ui.tex {
                c.painter().image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    })
}

/// Ask for a package. The dialog runs modally, so not from inside the window's frame: macOS
/// sends the window events while it is open (a mouse leaving it), and the window's code is still
/// in the frame then (in GarageBand's AU host that aborts). On macOS it opens from the main queue
/// once the frame is done; the answer lands in the returned slot (a path, or None if cancelled).
type Answer = Arc<std::sync::Mutex<Option<Option<std::path::PathBuf>>>>;
type Job = Arc<std::sync::Mutex<Option<Result<String, String>>>>;
#[derive(Clone, Copy)]
enum FileAction { Load, Export }

fn pick(action: FileAction, name: &str) -> Answer {
    let answer: Answer = Default::default();
    let slot = answer.clone();
    let name: String = name.chars().take(100).map(|c|
        if c.is_alphanumeric() || " _-".contains(c) { c } else { '_' }).collect();
    let ask = move || {
        let path = match action {
            FileAction::Load => rfd::FileDialog::new()
                .add_filter("FM-1 firmware or shared save", &["fwsc", "fm1save", "json"]).pick_file(),
            FileAction::Export => rfd::FileDialog::new().add_filter("FM-1 shared save", &["fm1save"])
                .set_file_name(format!("{}.fm1save", if name.is_empty() { "FM-1" } else { &name })).save_file(),
        };
        *slot.lock().unwrap() = Some(path);
    };
    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(ask);
    #[cfg(not(target_os = "macos"))]
    std::thread::spawn(ask);
    answer
}

// File I/O, hashing and serialization never run in the audio callback or window frame.
// The machine acknowledges imports only after a validated boot has replaced the old setup.
fn file_job(action: FileAction, mut path: std::path::PathBuf, params: Arc<Fm1Params>, link: Arc<Link>) -> Job {
    let job: Job = Default::default();
    let result = job.clone();
    std::thread::spawn(move || {
        if matches!(action, FileAction::Export) && path.extension().is_none() { path.set_extension("fm1save"); }
        let run = || -> Result<String, String> {
            match action {
                FileAction::Load => {
                    let current = params.firmware.lock().unwrap().clone();
                    let data = crate::savefile::read_with_firmware(&path, Some(&current))?;
                    let firmware = data.firmware.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    link.cmds.send(Cmd::Load { firmware: data.firmware, sectors: data.sectors, reply: tx })
                        .map_err(|_| "the machine stopped".to_owned())?;
                    rx.recv().map_err(|_| "the machine stopped while loading".to_owned())??;
                    *link.running.lock().unwrap() = Some(firmware.pkg);
                    Ok(format!("Loaded {}", firmware.name))
                }
                FileAction::Export => {
                    let firmware = params.firmware.lock().unwrap().clone();
                    if firmware.pkg.is_empty() { return Err("load a firmware first".into()); }
                    let (tx, rx) = std::sync::mpsc::channel();
                    link.cmds.send(Cmd::Snapshot { pkg: firmware.pkg.clone(), reply: tx })
                        .map_err(|_| "the machine stopped".to_owned())?;
                    let sectors = rx.recv().map_err(|_| "the machine stopped while exporting".to_owned())??;
                    crate::savefile::write(&path, &crate::savefile::SaveData { firmware, sectors })?;
                    Ok(format!("Exported {}", path.file_name().unwrap_or_default().to_string_lossy()))
                }
            }
        };
        *result.lock().unwrap() = Some(run());
    });
    job
}

/// fm1-web's `panel_hit` codes: -1 nothing, 0..6 an encoder (FM-1; SMK-37 0..7 K1..K8), 7 the
/// FM-1's pot, 100 + id a button or key, SMK-37: 200 + p a pad, 300 + f a fader, 400 / 401 the
/// pitch / mod wheel.
fn hit(smk: bool, x: f32, y: f32) -> i32 {
    if smk {
        return match panel_smk::hit(x, y) {
            Some(panel_smk::Ctl::Enc(i)) => i as i32,
            Some(panel_smk::Ctl::Btn(id)) | Some(panel_smk::Ctl::Key(id, _)) => 100 + id as i32,
            Some(panel_smk::Ctl::Pad(p)) => 200 + p as i32,
            Some(panel_smk::Ctl::Fader(f)) => 300 + f as i32,
            Some(panel_smk::Ctl::Pitch) => 400,
            Some(panel_smk::Ctl::Mod) => 401,
            None => -1,
        };
    }
    match panel::hit(x, y) {
        Some(panel::Ctl::Enc(i)) => i as i32,
        Some(panel::Ctl::Pot) => 7,
        Some(panel::Ctl::Btn(id)) | Some(panel::Ctl::Key(id)) => 100 + id as i32,
        None => -1,
    }
}

/// How far down a key the SMK-37's panel was hit (0 top .. 1 bottom).
fn panel_smk_hit_y(x: f32, y: f32) -> f32 {
    match panel_smk::hit(x, y) {
        Some(panel_smk::Ctl::Key(_, f)) => f,
        _ => 0.5,
    }
}

fn key(ui: &mut Ui, link: &Link, id: i32, down: bool) {
    if !(0..64).contains(&id) || ui.down[id as usize] == down {
        return;
    }
    ui.down[id as usize] = down;
    let _ = link.cmds.send(Cmd::Key { id: id as u8, down });
}

fn turn(ui: &mut Ui, link: &Link, smk: bool, i: i32, clicks: i32) {
    ui.enc_clicks[i as usize] += clicks;
    ui.turning[i as usize] = 10;
    let enc = if smk { i as u8 } else { panel::ENCS[i as usize].1 };
    let _ = link.cmds.send(Cmd::Enc { enc, clicks });
}

fn analog(ui: &mut Ui, link: &Link, i: usize, v: u16) {
    if ui.analog[i] != v {
        ui.analog[i] = v;
        let _ = link.cmds.send(Cmd::Analog { idx: i as u8, value: v });
    }
}

/// SMK-37: a pad pressed, a fader or a wheel dragged.
fn slide(ui: &mut Ui, link: &Link, ctl: i32, y: f32) {
    if ctl < 300 {
        analog(ui, link, (ctl - 200) as usize, 700);
    } else if ctl < 400 {
        let f = (ctl - 300) as usize;
        analog(ui, link, 16 + f, panel_smk::fader_value(f, y));
    } else {
        let pitch = ctl == 400;
        analog(ui, link, if pitch { 20 } else { 21 }, panel_smk::wheel_value(pitch, y));
    }
}

/// SMK-37: the next keys' velocity, from how far down the key was hit (fm1-web's curve).
fn velocity(link: &Link, f: f32) {
    let us = (80000f32.ln() + (2000f32.ln() - 80000f32.ln()) * f.clamp(0.0, 1.0)).exp();
    let _ = link.cmds.send(Cmd::Vel(us as u32));
}
