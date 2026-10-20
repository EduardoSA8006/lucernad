//! Uso da CPU pelo `/proc/stat`.

/// Os contadores da linha `cpu` do `/proc/stat`, em jiffies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuTimes {
    /// `idle` + `iowait` (campos 3 e 4).
    pub idle: u64,
    /// A soma de todos os campos.
    pub total: u64,
}

/// Lê a primeira linha (`cpu …`) do `/proc/stat`; `None` se ela não for a
/// linha agregada da CPU com números.
pub fn parse_stat(text: &str) -> Option<CpuTimes> {
    let mut words = text.lines().next()?.split_whitespace();
    if words.next()? != "cpu" {
        return None;
    }
    let fields = words
        .map(|w| w.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let idle = fields
        .get(3)?
        .saturating_add(fields.get(4).copied().unwrap_or(0));
    let total = fields.iter().fold(0u64, |a, b| a.saturating_add(*b));
    Some(CpuTimes { idle, total })
}

/// O uso (0 a 1) entre duas leituras; `None` se o total não cresceu.
pub fn cpu_usage(prev: CpuTimes, cur: CpuTimes) -> Option<f64> {
    if cur.total <= prev.total {
        return None;
    }
    let total = (cur.total - prev.total) as f64;
    let idle = cur.idle.saturating_sub(prev.idle) as f64;
    Some((1.0 - idle / total).clamp(0.0, 1.0))
}
