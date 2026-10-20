//! Rede pelo `/proc/net/dev`: totais, taxas e o histórico.

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

/// Soma o `/proc/net/dev` das interfaces físicas (pula as 2 linhas de
/// cabeçalho; `rx` é o campo 0 e `tx` o 8 depois do `:`).
pub fn parse_net_dev(text: &str) -> NetTotals {
    let mut totals = NetTotals::default();
    for line in text.lines().skip(2) {
        let Some((name, data)) = line.split_once(':') else {
            continue;
        };
        if !is_physical(name.trim()) {
            continue;
        }
        let fields: Vec<u64> = data
            .split_whitespace()
            .map(|w| w.parse().unwrap_or(0))
            .collect();
        let field = |i: usize| fields.get(i).copied().unwrap_or(0);
        totals.rx = totals.rx.saturating_add(field(0));
        totals.tx = totals.tx.saturating_add(field(8));
    }
    totals
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
