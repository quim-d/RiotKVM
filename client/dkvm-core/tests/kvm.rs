//! Port de las pruebas Python (layouts, teclas, Kvm, macros). Sin hardware.

use dkvm_core::input::{abs_from_pixels, Kvm, MouseCoalescer};
use dkvm_core::keys::parse_combo;
use dkvm_core::layouts::get_layout;
use dkvm_core::script::{parse_script, run_script, Context};
use dkvm_core::transport::mock::MockEvent;
use dkvm_core::{Error, MockHandle, MockTransport};

fn kvm(layout: &str) -> (Kvm, MockHandle) {
    let (t, h) = MockTransport::with_handle();
    let mut k = Kvm::new(Box::new(t), layout).unwrap();
    k.set_sleep(|_| {});
    (k, h)
}

fn sent(h: &MockHandle) -> Vec<String> {
    h.lock().sent.clone()
}

// ---------- LayoutTests ----------
#[test]
fn layout_us() {
    let us = get_layout("us").unwrap();
    assert_eq!(us.strokes('a').unwrap(), vec![(0x04, 0)]);
    assert_eq!(us.strokes('A').unwrap(), vec![(0x04, 0x02)]);
    assert_eq!(us.strokes('@').unwrap(), vec![(0x1F, 0x02)]);
    assert_eq!(us.strokes(':').unwrap(), vec![(0x33, 0x02)]);
}

#[test]
fn layout_es() {
    let es = get_layout("es").unwrap();
    assert_eq!(es.strokes('@').unwrap(), vec![(0x1F, 0x40)]); // AltGr+2
    assert_eq!(es.strokes('ñ').unwrap(), vec![(0x33, 0)]);
    assert_eq!(es.strokes('"').unwrap(), vec![(0x1F, 0x02)]); // Shift+2
    assert_eq!(es.strokes('{').unwrap(), vec![(0x34, 0x40)]); // AltGr+´
    assert_eq!(es.strokes('\\').unwrap(), vec![(0x35, 0x40)]); // AltGr+º
    assert_eq!(es.strokes('-').unwrap(), vec![(0x38, 0)]);
    assert_eq!(es.strokes('á').unwrap(), vec![(0x34, 0), (0x04, 0)]); // tecla muerta + a
    assert_eq!(es.strokes('Ü').unwrap(), vec![(0x34, 0x02), (0x18, 0x02)]);
    assert_eq!(es.strokes('`').unwrap(), vec![(0x2F, 0), (0x2C, 0)]);
}

#[test]
fn layout_unsupported() {
    assert_eq!(get_layout("us").unwrap().strokes('ñ').unwrap_err().exit_code(), 2);
    assert_eq!(get_layout("es").unwrap().strokes('漢').unwrap_err().exit_code(), 2);
    assert_eq!(get_layout("xx").err().unwrap().exit_code(), 2);
}

#[test]
fn layout_all_ascii_printable_covered() {
    for name in ["us", "es"] {
        let lay = get_layout(name).unwrap();
        for c in 32u8..127 {
            lay.strokes(c as char).unwrap_or_else(|e| panic!("{name} {}: {e}", c as char));
        }
    }
}

// ---------- KeysTests ----------
#[test]
fn keys_combo() {
    let es = get_layout("es").unwrap();
    assert_eq!(parse_combo("ctrl+alt+del", es).unwrap(), vec![(0xE0, 0), (0xE2, 0), (0x4C, 0)]);
    assert_eq!(parse_combo("F5", es).unwrap(), vec![(0x3E, 0)]);
    assert_eq!(parse_combo("win+r", es).unwrap(), vec![(0xE3, 0), (0x15, 0)]);
    assert_eq!(parse_combo("ctrl+@", es).unwrap(), vec![(0xE0, 0), (0x1F, 0x40)]);
    assert_eq!(*parse_combo("ctrl++", es).unwrap().last().unwrap(), es.strokes('+').unwrap()[0]);
}

#[test]
fn keys_errors() {
    let es = get_layout("es").unwrap();
    for bad in ["", "ctrl+", "ctrl+nada", "0xZZ"] {
        assert_eq!(parse_combo(bad, es).unwrap_err().exit_code(), 2, "{bad:?}");
    }
}

// ---------- KvmTests ----------
#[test]
fn type_text_us() {
    let (mut k, h) = kvm("us");
    k.type_text("Hi", 0.0).unwrap();
    assert_eq!(sent(&h), ["kd 0xE1", "kd 0x0B", "ku 0x0B", "ku 0xE1", "kd 0x0C", "ku 0x0C"]);
    assert!(h.lock().keys.is_empty());
}

#[test]
fn type_validates_before_sending() {
    let (mut k, h) = kvm("us");
    assert_eq!(k.type_text("ok ñ", 0.0).unwrap_err().exit_code(), 2);
    assert!(sent(&h).is_empty());
}

#[test]
fn newline_is_enter() {
    let (mut k, h) = kvm("es");
    k.type_text("a\r\nb", 0.0).unwrap();
    assert_eq!(h.lock().kd_usages(), [0x04, 0x28, 0x05]);
}

#[test]
fn press_releases_in_reverse() {
    let (mut k, h) = kvm("es");
    k.press("ctrl+alt+del", 0.0).unwrap();
    assert_eq!(sent(&h), ["kd 0xE0", "kd 0xE2", "kd 0x4C", "ku 0x4C", "ku 0xE2", "ku 0xE0"]);
    assert!(h.lock().keys.is_empty());
}

#[test]
fn press_releases_on_error() {
    let (mut k, h) = kvm("es");
    h.lock().fail_lines.insert("kd 0x4C".into(), "err boom".into());
    let e = k.press("ctrl+del", 0.0).unwrap_err();
    assert!(matches!(e, Error::Device(_)));
    assert!(sent(&h).contains(&"ku 0xE0".to_string()));
}

#[test]
fn move_rel_chunks() {
    let (mut k, h) = kvm("es");
    k.move_rel(300, -200, 0).unwrap();
    let st = h.lock();
    assert_eq!(st.rel_total, [300, -200]);
    assert!(st.events.iter().all(|e| matches!(e, MockEvent::Mm(x, y, _) if x.abs() <= 127 && y.abs() <= 127)));
}

#[test]
fn abs_mapping() {
    assert_eq!(abs_from_pixels(0.0, 0.0, 1920, 1080).unwrap(), (0, 0));
    assert_eq!(abs_from_pixels(1919.0, 1079.0, 1920, 1080).unwrap(), (32767, 32767));
    assert_eq!(abs_from_pixels(5000.0, -4.0, 1920, 1080).unwrap(), (32767, 0));
    let (x, _) = abs_from_pixels(960.0, 540.0, 1920, 1080).unwrap();
    assert!((x - 16384).abs() <= 10);
}

#[test]
fn click_and_buttons() {
    let (mut k, h) = kvm("es");
    k.click("right", 2, false, 0.0).unwrap();
    assert_eq!(sent(&h), ["mba 2", "mba 0", "mba 2", "mba 0"]);
    k.click("left", 1, true, 0.0).unwrap();
    assert_eq!(sent(&h)[4..], ["mbr 1", "mbr 0"]);
    k.button_down("left", false).unwrap();
    k.button_down("middle", false).unwrap();
    assert_eq!(h.lock().abs_buttons, 5);
    k.release_all().unwrap();
    let st = h.lock();
    assert_eq!((st.abs_buttons, st.rel_buttons, k.buttons), (0, 0, 0));
}

#[test]
fn scroll_abs_keeps_position() {
    let (mut k, h) = kvm("es");
    k.move_abs(100, 200).unwrap();
    k.scroll(-3, false).unwrap();
    assert_eq!(*h.lock().events.last().unwrap(), MockEvent::Ma(100, 200, -3));
}

#[test]
fn status_is_ready() {
    let (mut k, _) = kvm("es");
    assert_eq!(k.status().unwrap()["hidready"], true);
}

#[test]
fn unknown_command_is_error() {
    let (mut k, _) = kvm("es");
    assert_eq!(k.raw("nope").unwrap_err().exit_code(), 4);
}

#[test]
fn deferred_error_surfaces_at_flush() {
    let (mut k, h) = kvm("es");
    k.auto_flush = false;
    h.lock().fail_next = Some("err x".into());
    k.key_down(4).unwrap(); // no espera el ok
    assert_eq!(k.flush().unwrap_err().exit_code(), 4);
    assert!(k.flush().is_ok());
}

#[test]
fn mouse_coalescer_merges_ticks() {
    let (mut k, h) = kvm("es");
    let mut c = MouseCoalescer::default();
    for _ in 0..50 {
        c.add_rel(2, -1);
    }
    c.set_abs(10, 20);
    c.set_abs(30, 40);
    c.add_wheel(-1);
    c.flush_into(&mut k).unwrap();
    assert!(c.is_empty());
    assert_eq!(sent(&h), ["ma 30 40 0", "mm 100 -50 -1"]);
}

// ---------- ScriptTests ----------
const SCRIPT: &str = "# demo\nlayout us\nscreen 1920x1080\nmouse-mode abs\nmove 960 540\nclick left 2\ntype \"Hi there\"\nkey ctrl+a\nsleep 0.1\nstatus\n";

#[test]
fn script_run() {
    let (mut k, h) = kvm("es");
    let mut ctx = Context::new(&mut k);
    run_script(&mut ctx, SCRIPT, None).unwrap();
    assert_eq!(ctx.results[0].0, "status");
    let s = sent(&h);
    assert_eq!(s[s.len() - 3..], ["kall", "mbr 0", "mba 0"]); // liberación final
    let st = h.lock();
    assert!(st.keys.is_empty());
    assert!(matches!(st.events[0], MockEvent::Ma(..)));
}

#[test]
fn script_errors_have_line_numbers_and_release() {
    let (mut k, h) = kvm("es");
    let mut ctx = Context::new(&mut k);
    let e = run_script(&mut ctx, "keydown shift\nfoo bar\n", None).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert!(e.to_string().contains("línea 2"), "{e}");
    assert!(h.lock().keys.is_empty()); // shift soltado pese al fallo
}

#[test]
fn script_bad_quotes() {
    assert_eq!(parse_script("type \"sin cerrar").unwrap_err().exit_code(), 2);
}

#[test]
fn type_fw_splits_long_lines_on_char_boundaries() {
    let (mut k, h) = kvm("es");
    let long = "ñ".repeat(300); // 600 bytes
    k.type_text_firmware(&long).unwrap();
    let st = h.lock();
    assert!(st.typed.len() >= 3);
    assert!(st.typed.iter().all(|c| c.len() <= 240));
    assert_eq!(st.typed.concat(), long);
}

#[test]
fn info_and_stats_on_mock() {
    let (mut k, _) = kvm("es");
    assert!(k.info().unwrap().starts_with("fw="));
    assert_eq!(k.stats(false).unwrap().lines().count(), 2);
    assert_eq!(k.stats(true).unwrap(), "ok");
}