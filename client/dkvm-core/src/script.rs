//! Macros reproducibles `.dkvm`: un fichero de texto, una acción por línea.
//!
//! ```text
//! # comentario
//! layout es                # es | us
//! mouse-mode abs           # abs | rel (afecta a move/click/down/up/scroll)
//! screen 1920x1080         # en modo abs, move X Y se interpreta en píxeles; sin esto, 0..32767
//! type "Hola, mundo"       # texto con el layout; las comillas son shlex
//! key ctrl+alt+del         # pulsar y soltar
//! keydown shift            # mantener / keyup shift
//! move 960 540             # abs: posición; rel: desplazamiento
//! click left 2             # botón [veces]
//! down left / up left      # mantener / soltar botón
//! scroll -3                # rueda (positivo = arriba)
//! sleep 0.5                # segundos
//! release                  # suelta todas las teclas y botones
//! status
//! raw kd 0x04              # orden cruda del protocolo del dongle
//! ```

use crate::error::{pyrepr, usage, Error, Result};
use crate::input::{parse_screen, Kvm};
use crate::layouts::get_layout;
use serde_json::Value;

pub struct Context<'a> {
    pub kvm: &'a mut Kvm,
    pub relative: bool,
    pub screen: Option<(i64, i64)>,
    /// Salida de `status`/`raw`: (orden, valor).
    pub results: Vec<(String, Value)>,
}

impl<'a> Context<'a> {
    pub fn new(kvm: &'a mut Kvm) -> Context<'a> {
        Context { kvm, relative: false, screen: None, results: Vec::new() }
    }
}

pub fn do_move(kvm: &mut Kvm, x: f64, y: f64, relative: bool, screen: Option<(i64, i64)>) -> Result<()> {
    if relative {
        kvm.move_rel(x as i64, y as i64, 0)
    } else if let Some((w, h)) = screen {
        kvm.move_px(x, y, w, h)
    } else {
        kvm.move_abs(x as i64, y as i64)
    }
}

fn need(tok: &[String], lo: usize, hi: usize, usage_txt: &str) -> Result<()> {
    let n = tok.len() - 1;
    if !(lo..=hi).contains(&n) {
        return usage(format!("uso: {usage_txt}"));
    }
    Ok(())
}

fn num_f(s: &str) -> Result<f64> {
    s.trim().parse::<f64>().map_err(|_| Error::Usage(format!("argumento inválido ({})", pyrepr(s))))
}

fn num_i(s: &str) -> Result<i64> {
    s.trim().parse::<i64>().map_err(|_| Error::Usage(format!("argumento inválido ({})", pyrepr(s))))
}

pub fn execute(ctx: &mut Context, tok: &[String]) -> Result<()> {
    let cmd = tok[0].to_lowercase();
    match cmd.as_str() {
        "type" => {
            need(tok, 1, 99, "type TEXTO")?;
            ctx.kvm.type_text(&tok[1..].join(" "), 0.0)
        }
        "key" => {
            need(tok, 1, 99, "key COMBO...")?;
            for c in &tok[1..] {
                ctx.kvm.press(c, 0.0)?;
            }
            Ok(())
        }
        "keydown" => {
            need(tok, 1, 1, "keydown COMBO")?;
            ctx.kvm.key_down_name(&tok[1])
        }
        "keyup" => {
            need(tok, 1, 1, "keyup COMBO")?;
            ctx.kvm.key_up_name(&tok[1])
        }
        "move" => {
            need(tok, 2, 2, "move X Y")?;
            let (x, y) = (num_f(&tok[1])?, num_f(&tok[2])?);
            do_move(ctx.kvm, x, y, ctx.relative, ctx.screen)
        }
        "click" => {
            need(tok, 0, 2, "click [BOTON] [VECES]")?;
            let btn = tok.get(1).map(String::as_str).unwrap_or("left");
            let n = match tok.get(2) {
                Some(s) => num_i(s)?,
                None => 1,
            };
            ctx.kvm.click(btn, n, ctx.relative, 0.0)
        }
        "down" | "up" => {
            need(tok, 0, 1, &format!("{cmd} [BOTON]"))?;
            let btn = tok.get(1).map(String::as_str).unwrap_or("left");
            if cmd == "down" {
                ctx.kvm.button_down(btn, ctx.relative)
            } else {
                ctx.kvm.button_up(btn, ctx.relative)
            }
        }
        "scroll" => {
            need(tok, 1, 1, "scroll N")?;
            let n = num_i(&tok[1])?;
            ctx.kvm.scroll(n, ctx.relative)
        }
        "sleep" => {
            need(tok, 1, 1, "sleep SEGUNDOS")?;
            let s = num_f(&tok[1])?;
            ctx.kvm.sleep(s);
            Ok(())
        }
        "release" => ctx.kvm.release_all(),
        "status" => {
            let s = ctx.kvm.status()?;
            ctx.results.push(("status".into(), Value::Object(s)));
            Ok(())
        }
        "raw" => {
            need(tok, 1, 99, "raw ORDEN")?;
            let r = ctx.kvm.raw(&tok[1..].join(" "))?;
            ctx.results.push(("raw".into(), Value::String(r)));
            Ok(())
        }
        "layout" => {
            need(tok, 1, 1, "layout es|us")?;
            ctx.kvm.layout = get_layout(&tok[1])?;
            Ok(())
        }
        "mouse-mode" => {
            need(tok, 1, 1, "mouse-mode abs|rel")?;
            if tok[1] != "abs" && tok[1] != "rel" {
                return usage("mouse-mode: abs o rel");
            }
            ctx.relative = tok[1] == "rel";
            Ok(())
        }
        "screen" => {
            need(tok, 1, 1, "screen ANCHOxALTO")?;
            ctx.screen = Some(parse_screen(&tok[1])?);
            Ok(())
        }
        _ => usage(format!("orden de script desconocida: {}", pyrepr(&cmd))),
    }
}

/// `shlex.split(s, comments=True)` de Python (modo POSIX). Un `#` sin entrecomillar inicia comentario.
pub fn shlex_split(s: &str) -> std::result::Result<Vec<String>, String> {
    #[derive(PartialEq)]
    enum St {
        Space,
        Word,
        Single,
        Double,
    }
    let mut out: Vec<String> = Vec::new();
    let mut tok = String::new();
    let mut quoted = false;
    let mut st = St::Space;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match st {
            St::Space | St::Word => {
                if c.is_whitespace() {
                    if !tok.is_empty() || quoted {
                        out.push(std::mem::take(&mut tok));
                    }
                    quoted = false;
                    st = St::Space;
                } else if c == '#' {
                    // el resto de la línea es comentario
                    if !tok.is_empty() || quoted {
                        out.push(std::mem::take(&mut tok));
                    }
                    return Ok(out);
                } else if c == '\\' {
                    match it.next() {
                        Some(n) => tok.push(n),
                        None => return Err("No escaped character".into()),
                    }
                    st = St::Word;
                } else if c == '\'' {
                    quoted = true;
                    st = St::Single;
                } else if c == '"' {
                    quoted = true;
                    st = St::Double;
                } else {
                    tok.push(c);
                    st = St::Word;
                }
            }
            St::Single => {
                if c == '\'' {
                    st = St::Word;
                } else {
                    tok.push(c);
                }
            }
            St::Double => {
                if c == '"' {
                    st = St::Word;
                } else if c == '\\' {
                    match it.next() {
                        Some(n) => {
                            if n != '\\' && n != '"' {
                                tok.push('\\');
                            }
                            tok.push(n);
                        }
                        None => return Err("No escaped character".into()),
                    }
                } else {
                    tok.push(c);
                }
            }
        }
    }
    if st == St::Single || st == St::Double {
        return Err("No closing quotation".into());
    }
    if !tok.is_empty() || quoted {
        out.push(tok);
    }
    Ok(out)
}

/// Devuelve [(nº de línea, tokens)] ignorando vacías y comentarios. Valida las comillas.
pub fn parse_script(text: &str) -> Result<Vec<(usize, Vec<String>)>> {
    let mut out = Vec::new();
    let norm = text.replace("\r\n", "\n").replace('\r', "\n");
    for (i, line) in norm.split('\n').enumerate() {
        let n = i + 1;
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        let tok = shlex_split(s).map_err(|e| Error::Usage(format!("línea {n}: {e}")))?;
        if !tok.is_empty() {
            out.push((n, tok));
        }
    }
    Ok(out)
}

/// Ejecuta el script. Al acabar (o fallar) suelta teclas y botones.
pub fn run_script(ctx: &mut Context, text: &str, mut on_step: Option<&mut dyn FnMut(usize, &[String])>) -> Result<()> {
    let steps = parse_script(text)?;
    let r = (|| -> Result<()> {
        for (n, tok) in &steps {
            if let Some(f) = on_step.as_mut() {
                f(*n, tok);
            }
            if let Err(e) = execute(ctx, tok) {
                return Err(match e {
                    Error::Usage(m) => Error::Usage(format!("línea {n}: {m}")),
                    other => other,
                });
            }
        }
        Ok(())
    })();
    let _ = ctx.kvm.release_all();
    r
}
