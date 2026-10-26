//! O casamento de faixas: decide se a faixa que uma fonte devolveu é a que
//! foi pedida, e dá a chave do cache.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// Diferença máxima de duração (em segundos) entre o pedido e a fonte.
pub const DURATION_TOLERANCE: f64 = 2.0;

/// A faixa pedida.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration: Option<f64>,
}

impl Track {
    /// A chave do cache: artista normalizado, título sem os sufixos e duração
    /// arredondada em segundos (ou `-`), separados por `\u{1f}`.
    pub fn key(&self) -> String {
        let duration = match self.duration {
            Some(d) if d.is_finite() && d >= 0.0 => format!("{}", d.round() as u64),
            _ => "-".to_string(),
        };
        format!(
            "{}\u{1f}{}\u{1f}{}",
            normalize(&self.artist),
            title_key(&self.title),
            duration
        )
    }
}

/// NFD sem marcas combinantes, minúsculas, o que não é letra nem número vira
/// espaço e os espaços são juntados.
pub fn normalize(s: &str) -> String {
    let mapped: String = s
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Palavras inteiras que marcam um sufixo que só muda a gravação.
const SUFFIX_WORDS: &[&str] = &[
    "live", "version", "versao", "edit", "mono", "stereo", "deluxe", "bonus", "demo", "explicit",
    "feat", "ft", "part", "com",
];
/// Radicais: a palavra pode continuar (`remastered`, `acústica`).
const SUFFIX_STEMS: &[&str] = &["remaster", "remasterizad", "acustic", "acoustic"];

/// O trecho (já normalizado) é um sufixo ignorado?
fn is_suffix(chunk: &str) -> bool {
    let words: Vec<&str> = chunk.split(' ').collect();
    words.windows(2).any(|w| w == ["ao", "vivo"])
        || words
            .iter()
            .any(|w| SUFFIX_WORDS.contains(w) || SUFFIX_STEMS.iter().any(|s| w.starts_with(s)))
}

/// Tira os grupos entre parênteses ou colchetes que são sufixos.
fn strip_groups(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(len) = rest[open + 1..].find(close) else {
            break;
        };
        let inner = &rest[open + 1..open + 1 + len];
        out.push_str(&rest[..open]);
        if is_suffix(&normalize(inner)) {
            out.push(' ');
        } else {
            out.push_str(&rest[open..open + len + 2]);
        }
        rest = &rest[open + len + 2..];
    }
    out.push_str(rest);
    out
}

/// O título normalizado e sem os sufixos que só mudam a gravação.
pub fn title_key(s: &str) -> String {
    let mut title = strip_groups(s);
    while let Some(i) = title.rfind(" - ") {
        if !is_suffix(&normalize(&title[i + 3..])) {
            break;
        }
        title.truncate(i);
    }
    let tokens: Vec<&str> = title.split_whitespace().collect();
    let cut = tokens
        .iter()
        .skip(1)
        .position(|t| ["feat.", "ft.", "part."].contains(&t.to_lowercase().as_str()));
    let key = match cut {
        Some(i) => normalize(&tokens[..=i].join(" ")),
        None => normalize(&title),
    };
    if key.is_empty() { normalize(s) } else { key }
}

/// O primeiro artista (normalizado): corta em `,`, `&`, ` feat. `, ` ft. `,
/// ` part. ` e ` x `.
pub fn first_artist(s: &str) -> String {
    const SEPARATORS: &[&str] = &[",", "&", " feat. ", " ft. ", " part. ", " x "];
    let cut = (0..s.len())
        .filter(|i| s.is_char_boundary(*i))
        .find(|&i| {
            SEPARATORS.iter().any(|sep| {
                s.as_bytes()
                    .get(i..i + sep.len())
                    .is_some_and(|b| b.eq_ignore_ascii_case(sep.as_bytes()))
            })
        })
        .unwrap_or(s.len());
    normalize(&s[..cut])
}

fn same_artist(a: &str, b: &str) -> bool {
    let (na, nb) = (normalize(a), normalize(b));
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    let (fa, fb) = (first_artist(a), first_artist(b));
    !fa.is_empty() && fa == fb
}

/// A faixa que a fonte devolveu é a pedida? Título pelo [`title_key`], artista
/// pelo nome inteiro ou pelo primeiro; a duração só conta quando os dois lados
/// têm (`|a − b| ≤` [`DURATION_TOLERANCE`]).
pub fn same_track(want: &Track, title: &str, artist: &str, duration: Option<f64>) -> bool {
    if title_key(&want.title) != title_key(title) || !same_artist(&want.artist, artist) {
        return false;
    }
    match (want.duration, duration) {
        (Some(a), Some(b)) => (a - b).abs() <= DURATION_TOLERANCE,
        _ => true,
    }
}
