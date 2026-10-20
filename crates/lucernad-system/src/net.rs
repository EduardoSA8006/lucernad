//! Rede pelo `/proc/net/dev`: totais, taxas e o histórico.

use std::collections::BTreeMap;

use crate::{Roots, read};

/// Se o nome é de uma interface que pode contar: não é a `lo` e serve de
/// nome de pasta (não vazio, sem `/`, nem `.` ou `..`).
fn countable(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && name != "lo"
}

/// Se a interface é física: tem um dispositivo por trás
/// (`<sysfs>/class/net/<if>/device`) e não é a `lo`.
///
/// Túneis e interfaces virtuais (VPN como o `CloudflareWARP`, `wg*`, `tun*`,
/// `docker*`, `veth*`, `br-*`) não têm `device`; contá-los somaria o mesmo
/// tráfego duas vezes (o túnel e a placa por onde ele passa).
pub fn is_physical(roots: &Roots, name: &str) -> bool {
    countable(name)
        && roots
            .sysfs
            .join("class/net")
            .join(name)
            .join("device")
            .exists()
}

/// Bytes recebidos e enviados, somados das interfaces físicas.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetTotals {
    pub rx: u64,
    pub tx: u64,
}

/// Os totais de cada interface do `/proc/net/dev` para a qual `keep` diz sim
/// (pula as 2 linhas de cabeçalho; `rx` é o campo 0 e `tx` o 8 depois do
/// `:`; campo que não é número conta 0).
pub fn parse_interfaces(text: &str, keep: impl Fn(&str) -> bool) -> BTreeMap<String, NetTotals> {
    let mut ifaces = BTreeMap::new();
    for line in text.lines().skip(2) {
        let Some((name, data)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !keep(name) {
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

/// A soma dos totais das interfaces.
pub fn sum(ifaces: &BTreeMap<String, NetTotals>) -> NetTotals {
    ifaces
        .values()
        .fold(NetTotals::default(), |a, t| NetTotals {
            rx: a.rx.saturating_add(t.rx),
            tx: a.tx.saturating_add(t.tx),
        })
}

/// As interfaces que contam no `<procfs>/net/dev`: as físicas
/// ([`is_physical`]); se nenhuma tiver `device` (um container só com `veth`,
/// um `ppp0` sozinho), todas menos a `lo`, para a rede não marcar 0. Vazio se
/// o arquivo não puder ser lido.
pub fn read_interfaces(roots: &Roots) -> BTreeMap<String, NetTotals> {
    let text = read(&roots.procfs.join("net/dev")).unwrap_or_default();
    let all = parse_interfaces(&text, countable);
    let physical: BTreeMap<_, _> = all
        .iter()
        .filter(|(name, _)| is_physical(roots, name))
        .map(|(n, t)| (n.clone(), *t))
        .collect();
    if physical.is_empty() { all } else { physical }
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
