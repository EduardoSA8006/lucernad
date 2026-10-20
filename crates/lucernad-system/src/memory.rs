//! Memória e swap pelo `/proc/meminfo`.

/// Totais e usados, em bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Memory {
    pub mem_total: u64,
    /// `MemTotal` − `MemAvailable`.
    pub mem_used: u64,
    pub swap_total: u64,
    /// `SwapTotal` − `SwapFree`.
    pub swap_used: u64,
}

/// Lê o `/proc/meminfo` (valores em kB); a chave que falta conta 0.
pub fn parse_meminfo(text: &str) -> Memory {
    let kb = |key: &str| -> u64 {
        text.lines()
            .find_map(|line| {
                let (k, rest) = line.split_once(':')?;
                if k != key {
                    return None;
                }
                rest.split_whitespace().next()?.parse::<u64>().ok()
            })
            .unwrap_or(0)
            .saturating_mul(1024)
    };
    let mem_total = kb("MemTotal");
    let swap_total = kb("SwapTotal");
    Memory {
        mem_total,
        mem_used: mem_total.saturating_sub(kb("MemAvailable")),
        swap_total,
        swap_used: swap_total.saturating_sub(kb("SwapFree")),
    }
}
