//! As opções do Hyprland: o valor lido pelo `j/getoption` e o Lua do
//! `hl.config({...})` que as muda (o Lucerna exige a config em Lua).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// O valor de uma opção, na forma do `j/getoption` (`{"bool": true}`,
/// `{"int": 0}`…), que é também a forma guardada no diário do modo jogo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// O valor da resposta do `j/getoption`. Erro com a resposta que não é o
/// JSON de uma opção (`no such option`) ou com um tipo que o modo jogo não
/// usa (gradiente, vetor).
pub fn parse_getoption(body: &str) -> Result<OptionValue, String> {
    let v: Value = serde_json::from_str(body).map_err(|_| body.trim().to_string())?;
    if let Some(b) = v.get("bool").and_then(Value::as_bool) {
        return Ok(OptionValue::Bool(b));
    }
    if let Some(i) = v.get("int").and_then(Value::as_i64) {
        return Ok(OptionValue::Int(i));
    }
    if let Some(f) = v.get("float").and_then(Value::as_f64) {
        return Ok(OptionValue::Float(f));
    }
    if let Some(s) = v.get("str").and_then(Value::as_str) {
        return Ok(OptionValue::Str(s.to_string()));
    }
    Err(format!("tipo de opção não suportado: {}", body.trim()))
}

/// Um nó da árvore do `hl.config`.
enum Node<'a> {
    Leaf(&'a OptionValue),
    Table(Vec<(&'a str, Node<'a>)>),
}

/// O Lua que muda as opções `changes` (nomes com `:`, como no
/// `getoption`) num `hl.config` só, aninhando pelo `:` na ordem dada.
pub fn config_lua(changes: &[(&str, OptionValue)]) -> String {
    let mut root: Vec<(&str, Node)> = Vec::new();
    for (name, value) in changes {
        let parts: Vec<&str> = name.split(':').collect();
        insert(&mut root, &parts, value);
    }
    format!("hl.config({})", table(&root))
}

fn insert<'a>(level: &mut Vec<(&'a str, Node<'a>)>, parts: &[&'a str], value: &'a OptionValue) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    if rest.is_empty() {
        level.push((first, Node::Leaf(value)));
        return;
    }
    let pos = level
        .iter()
        .position(|(k, n)| k == first && matches!(n, Node::Table(_)));
    let i = match pos {
        Some(i) => i,
        None => {
            level.push((first, Node::Table(Vec::new())));
            level.len() - 1
        }
    };
    if let Node::Table(inner) = &mut level[i].1 {
        insert(inner, rest, value);
    }
}

fn table(entries: &[(&str, Node)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(k, n)| {
            let v = match n {
                Node::Leaf(v) => lua_value(v),
                Node::Table(t) => table(t),
            };
            format!("{} = {v}", lua_key(k))
        })
        .collect();
    format!("{{ {} }}", items.join(", "))
}

const KEYWORDS: [&str; 22] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

fn lua_key(k: &str) -> String {
    let mut chars = k.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&k);
    if ident {
        k.to_string()
    } else {
        format!("[{}]", lua_string(k))
    }
}

fn lua_value(v: &OptionValue) -> String {
    match v {
        OptionValue::Bool(b) => b.to_string(),
        OptionValue::Int(i) => i.to_string(),
        OptionValue::Float(f) => format!("{f:?}"),
        OptionValue::Str(s) => lua_string(s),
    }
}

/// Um texto como string Lua entre aspas, com os escapes.
pub fn lua_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() && (c as u32) < 0x80 => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
