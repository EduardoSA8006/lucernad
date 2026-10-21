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
    let mut scan = Scan::new(&needle, false);
    for (i, c) in text.chars().enumerate() {
        if !scan.push(i, c) {
            break;
        }
    }
    scan.found
}

/// Onde a janela da prévia começa, pelo primeiro casamento.
fn window_start(first: usize) -> usize {
    if first > LATE_MATCH {
        first - CONTEXT_CHARS
    } else {
        0
    }
}

/// Passa a `f` cada caractere do texto com os espaços colapsados (como
/// `split_whitespace` + `join(" ")`) e a posição dele, até `f` devolver
/// `false`; sem montar a cópia inteira.
fn each_collapsed(text: &str, mut f: impl FnMut(usize, char) -> bool) {
    let mut at = 0;
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = at > 0;
            continue;
        }
        if space {
            space = false;
            if !f(at, ' ') {
                return;
            }
            at += 1;
        }
        if !f(at, c) {
            return;
        }
        at += 1;
    }
}

/// A forma normalizada de cada caractere, com o ASCII direto e os outros
/// guardados: o [`normalize`] de um caractere custa uma `String` e a NFD, e
/// um texto de 1 MiB repete os mesmos poucos acentos milhares de vezes.
struct Folder {
    cache: std::collections::HashMap<char, Box<str>>,
}

impl Folder {
    fn fold(&mut self, c: char, out: &mut impl FnMut(char) -> bool) -> bool {
        if c.is_ascii() {
            return out(c.to_ascii_lowercase());
        }
        let folded = self
            .cache
            .entry(c)
            .or_insert_with(|| normalize(c.encode_utf8(&mut [0; 4])).into());
        folded.chars().all(out)
    }
}

/// A busca de uma consulta (já normalizada) caractere a caractere, sem
/// sobreposição, até [`MAX_MATCHES`]. Com `window`, para no fim da janela da
/// prévia aberta pelo primeiro casamento: num texto de 1 MiB, só o primeiro
/// casamento pode exigir percorrer tudo.
struct Scan<'a> {
    needle: &'a [char],
    found: Vec<Range<usize>>,
    /// Os últimos caracteres normalizados (circular), com o original de cada um.
    recent: Vec<(char, usize)>,
    /// Quantos caracteres normalizados já passaram desde o último casamento.
    seen: usize,
    /// Sem sobreposição: depois de um casamento, segue do caractere seguinte.
    skip_until: usize,
    window: bool,
    limit: usize,
    folder: Folder,
}

impl<'a> Scan<'a> {
    fn new(needle: &'a [char], window: bool) -> Scan<'a> {
        Scan {
            needle,
            found: Vec::new(),
            recent: vec![('\0', 0); needle.len()],
            seen: 0,
            skip_until: 0,
            window,
            limit: usize::MAX,
            folder: Folder {
                cache: Default::default(),
            },
        }
    }

    /// O caractere `c`, na posição `i`; `false` quando não precisa de mais.
    fn push(&mut self, i: usize, c: char) -> bool {
        let n = self.needle.len();
        if n == 0 || i >= self.limit || self.found.len() >= MAX_MATCHES {
            return false;
        }
        if i < self.skip_until {
            return true;
        }
        let Scan {
            needle,
            found,
            recent,
            seen,
            skip_until,
            window,
            limit,
            folder,
        } = self;
        folder.fold(c, &mut |f| {
            recent[*seen % n] = (f, i);
            *seen += 1;
            if f != needle[n - 1] || *seen < n {
                return true;
            }
            let first = *seen - n;
            if (0..n).any(|k| recent[(first + k) % n].0 != needle[k]) {
                return true;
            }
            let start = recent[first % n].1;
            found.push(start..i + 1);
            *seen = 0;
            *skip_until = i + 1;
            if *window && found.len() == 1 {
                *limit = window_start(start) + PREVIEW_CHARS;
            }
            // O resto deste caractere já faz parte do casamento.
            false
        });
        true
    }
}

/// A prévia (espaços colapsados, janela) e os trechos marcados em UTF-16 dela.
pub fn preview_for(text: &str, query: Option<&str>) -> (String, Vec<[usize; 2]>) {
    let needle: Vec<char> = query
        .map(|q| normalize(q).chars().collect())
        .unwrap_or_default();
    let mut scan = Scan::new(&needle, true);
    each_collapsed(text, |i, c| scan.push(i, c));
    let matches = scan.found;
    let start = matches.first().map_or(0, |m| window_start(m.start));
    let mut window: Vec<char> = Vec::with_capacity(PREVIEW_CHARS);
    each_collapsed(text, |i, c| {
        if i >= start {
            window.push(c);
        }
        window.len() < PREVIEW_CHARS
    });
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
