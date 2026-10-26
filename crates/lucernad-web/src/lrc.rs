//! A letra: o LRC sincronizado e o texto simples viram uma lista de [`Line`].
//!
//! Texto vindo de fora, então tudo tem limite: [`LINES_MAX`] linhas, até
//! [`LINE_CHARS_MAX`] caracteres cada e [`TEXT_BYTES_MAX`] bytes de texto
//! somado. O que passa disso cai (a linha que estouraria a soma cai com as
//! seguintes). Caracteres de controle são removidos.

use serde_json::{Value, json};

/// Máximo de linhas de uma letra.
pub const LINES_MAX: usize = 1000;
/// Máximo de caracteres de uma linha.
pub const LINE_CHARS_MAX: usize = 200;
/// Máximo de texto somado, em bytes UTF-8.
pub const TEXT_BYTES_MAX: usize = 128 * 1024;

/// Uma linha da letra; `time` em segundos, ou `None` na letra de texto.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time: Option<f64>,
    pub text: String,
}

/// Lê um LRC. Só entram as linhas com ao menos uma marca de tempo válida
/// (`[mm:ss.xx]`); várias marcas repetem o texto; o `[offset:±ms]` desloca
/// tudo (positivo adianta); as demais etiquetas são ignoradas. A saída sai
/// ordenada por tempo (ordem estável).
pub fn parse_lrc(lrc: &str) -> Vec<Line> {
    let mut offset_ms = 0.0;
    let mut found: Vec<(f64, String)> = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim_start();
        let mut stamps = Vec::new();
        while let Some(tag_end) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
            let tag = &rest[1..=tag_end];
            if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            } else if let Some(ms) = parse_offset(tag) {
                offset_ms = ms;
            } else {
                break;
            }
            rest = rest[tag_end + 2..].trim_start();
        }
        if stamps.is_empty() {
            continue;
        }
        let text = clean(rest, true);
        for t in stamps {
            found.push((t, text.clone()));
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let lines = found
        .into_iter()
        .map(|(t, text)| Line {
            time: Some((t - offset_ms / 1000.0).max(0.0)),
            text,
        })
        .collect();
    limit(lines)
}

/// Uma letra de texto simples: `time` sempre `None`; `\r\n` aceito; as linhas
/// vazias do fim saem, as do meio ficam como `""`.
pub fn plain_lines(text: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| Line {
            time: None,
            text: clean(l, true),
        })
        .collect();
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    limit(lines)
}

/// As linhas no formato do tópico `web.lyrics`: `[{time, text}]`, com `time`
/// `null` na letra de texto.
pub fn lines_json(lines: &[Line]) -> Value {
    Value::Array(
        lines
            .iter()
            .map(|l| json!({"time": l.time, "text": l.text}))
            .collect(),
    )
}

/// `mm:ss`, `mm:ss.f`… (minutos de 1 a 3 dígitos, segundos de 2 dígitos abaixo
/// de 60, fração de 0 a 3 dígitos), em segundos.
fn parse_stamp(tag: &str) -> Option<f64> {
    let (min, rest) = tag.split_once(':')?;
    let (sec, frac) = match rest.split_once('.') {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let digits = |s: &str, lo: usize, hi: usize| {
        (lo..=hi).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(min, 1, 3) || !digits(sec, 2, 2) || !digits(frac, 0, 3) {
        return None;
    }
    let secs: f64 = sec.parse().ok()?;
    if secs >= 60.0 {
        return None;
    }
    let frac = if frac.is_empty() {
        0.0
    } else {
        frac.parse::<f64>().ok()? / 10f64.powi(frac.len() as i32)
    };
    Some(min.parse::<f64>().ok()? * 60.0 + secs + frac)
}

/// `offset:+500`, em milissegundos.
fn parse_offset(tag: &str) -> Option<f64> {
    let (name, value) = tag.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("offset") {
        return None;
    }
    let value = value.trim();
    let value = value.strip_prefix('+').unwrap_or(value);
    value.parse::<i64>().ok().map(|v| v as f64)
}

/// Tira os caracteres de controle (e as pontas, se `trim`).
fn clean(s: &str, trim: bool) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    if trim { s.trim().to_string() } else { s }
}

/// Aplica os três limites, em ordem.
fn limit(lines: Vec<Line>) -> Vec<Line> {
    let mut total = 0;
    let mut out = Vec::new();
    for mut line in lines.into_iter().take(LINES_MAX) {
        if let Some((end, _)) = line.text.char_indices().nth(LINE_CHARS_MAX) {
            line.text.truncate(end);
        }
        total += line.text.len();
        if total > TEXT_BYTES_MAX {
            break;
        }
        out.push(line);
    }
    out
}
