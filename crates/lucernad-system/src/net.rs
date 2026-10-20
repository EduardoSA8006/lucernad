//! Rede pelo `/proc/net/dev`: totais, taxas e o histórico.

use std::collections::BTreeMap;

/// Prefixos de loopback e de interfaces virtuais (containers, bridges, VPN).
const VIRTUAL: [&str; 10] = [
    "lo", "docker", "veth", "br-", "virbr", "vnet", "tun", "tap", "wg", "zt",
];

/// Se a interface conta nos totais (não é loopback nem virtual).
pub fn is_physical(name: &str) -> bool {
    !VIRTUAL.iter().any(|p| name.starts_with(p))
}

/// Bytes recebidos e enviados, somados das interfaces físicas.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetTotals {
    pub rx: u64,
    pub tx: u64,
}

/// Os totais de cada interface física do `/proc/net/dev` (pula as 2 linhas
/// de cabeçalho; `rx` é o campo 0 e `tx` o 8 depois do `:`; campo que não é
/// número conta 0).
pub fn parse_interfaces(text: &str) -> BTreeMap<String, NetTotals> {
    let mut ifaces = BTreeMap::new();
    for line in text.lines().skip(2) {
        let Some((name, data)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !is_physical(name) {
            continue;
        }
        let fields: Vec<u64> = data
            .split_whitespace()
            .map(|w| w.parse().unwrap_or(0))
            .collect();
        let field = |i: usize| fields.get(i).copied().unwrap_or(0);
        ifaces.insert(
            name.to_string(),
            NetTotals {
                rx: field(0),
                tx: field(8),
            },
        );
    }
    ifaces
}

/// Soma o `/proc/net/dev` das interfaces físicas ([`parse_interfaces`]).
pub fn parse_net_dev(text: &str) -> NetTotals {
    parse_interfaces(text)
        .values()
        .fold(NetTotals::default(), |a, t| NetTotals {
            rx: a.rx.saturating_add(t.rx),
            tx: a.tx.saturating_add(t.tx),
        })
}

/// As taxas de download e upload (bytes/s) entre duas leituras por
/// interface: a soma das [`rate`] de cada interface presente nas duas. Uma
/// interface que aparece ou some na janela não entra, para não virar pico
/// (o total dela inteiro) nem zerar a taxa das outras.
pub fn rates(
    prev: &BTreeMap<String, NetTotals>,
    cur: &BTreeMap<String, NetTotals>,
    secs: f64,
) -> (f64, f64) {
    cur.iter()
        .filter_map(|(name, c)| Some((prev.get(name)?, c)))
        .fold((0.0, 0.0), |(rx, tx), (p, c)| {
            (rx + rate(p.rx, c.rx, secs), tx + rate(p.tx, c.tx, secs))
        })
}

/// Bytes por segundo entre duas leituras; `0.0` com janela vazia ou contador
/// que voltou (nunca `NaN`).
pub fn rate(prev: u64, cur: u64, secs: f64) -> f64 {
    if secs.is_nan() || secs <= 0.0 || cur < prev {
        return 0.0;
    }
    (cur - prev) as f64 / secs
}

/// Quantos pontos o histórico das taxas guarda.
pub const HISTORY: usize = 30;

/// Põe `value` no fim do histórico, largando os mais velhos além de
/// [`HISTORY`].
pub fn push_history(list: &mut Vec<f64>, value: f64) {
    list.push(value);
    if list.len() > HISTORY {
        let extra = list.len() - HISTORY;
        list.drain(..extra);
    }
}
