//! A busca sem acentos e o trecho marcado, tudo puro.
//!
//! A mesma regra de normalização serve ao índice, à busca curta e ao trecho
//! marcado. Os intervalos da prévia saem em unidades UTF-16, que é como o QML
//! corta o texto.

use std::ops::Range;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// O tamanho máximo da prévia, em caracteres.
pub const PREVIEW_CHARS: usize = 400;
/// Quantos caracteres antes do casamento a janela começa.
pub const CONTEXT_CHARS: usize = 40;
/// O casamento depois deste caractere faz a prévia começar mais perto dele.
pub const LATE_MATCH: usize = 300;
/// O máximo de trechos marcados por item.
pub const MAX_MATCHES: usize = 20;
/// A consulta com até este tamanho (normalizada) não usa o índice.
pub const SHORT_QUERY: usize = 2;

/// NFD, sem as marcas combinantes, em minúsculas.
pub fn normalize(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

/// O último segmento de cada `file://`, decodificado do `%XX`, separados por espaço.
pub fn file_names(uris: &[String]) -> String {
    uris.iter()
        .map(|uri| {
            let path = uri.strip_prefix("file://").unwrap_or(uri);
            let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            percent_decode(name)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Os casamentos de `query` em `text`, em caracteres do `text`, sem sobreposição.
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = normalize(query).chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    // Cada caractere normalizado guarda de que caractere original veio.
    let mut hay: Vec<char> = Vec::new();
    let mut origin: Vec<usize> = Vec::new();
    for (i, c) in text.chars().enumerate() {
        for n in normalize(c.encode_utf8(&mut [0; 4])).chars() {
            hay.push(n);
            origin.push(i);
        }
    }
    let mut found = Vec::new();
    let mut at = 0;
    while at + needle.len() <= hay.len() && found.len() < MAX_MATCHES {
        if hay[at..at + needle.len()] == needle[..] {
            let start = origin[at];
            let end = origin[at + needle.len() - 1] + 1;
            found.push(start..end);
            // Sem sobreposição: segue depois do último caractere original usado.
            while at < hay.len() && origin[at] < end {
                at += 1;
            }
        } else {
            at += 1;
        }
    }
    found
}

/// A prévia (espaços colapsados, janela) e os trechos marcados em UTF-16 dela.
pub fn preview_for(text: &str, query: Option<&str>) -> (String, Vec<[usize; 2]>) {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let matches = query
        .map(|q| find_matches(&collapsed, q))
        .unwrap_or_default();
    let start = match matches.first() {
        Some(m) if m.start > LATE_MATCH => m.start - CONTEXT_CHARS,
        _ => 0,
    };
    let window: Vec<char> = collapsed.chars().skip(start).take(PREVIEW_CHARS).collect();
    let prefix = usize::from(start > 0);
    let mut preview = String::new();
    if start > 0 {
        preview.push('…');
    }
    preview.extend(window.iter());

    // Posição em UTF-16 de cada caractere da janela, já contando o "…".
    let mut units = Vec::with_capacity(window.len() + 1);
    let mut acc = prefix;
    units.push(acc);
    for c in &window {
        acc += c.len_utf16();
        units.push(acc);
    }
    let ranges = matches
        .iter()
        .filter(|m| m.start >= start && m.end - start <= window.len())
        .map(|m| [units[m.start - start], units[m.end - start]])
        .collect();
    (preview, ranges)
}
