//! A descoberta das telas: a integrada pelo [`backlight`](crate::backlight)
//! e os monitores pelo [`drm`](crate::drm), com o barramento de cada um pelo
//! link `ddc`, pelo cache ou pelo EDID lido nos barramentos candidatos.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lucernad_core::{Detached, detached, panic_message};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{
    BUS_AMBIGUOUS, BUS_NOT_FOUND, Event, Monitor, NO_I2C_DEV, Panel, Screens, read_failed,
};
use crate::adjust::Adjuster;
use crate::backlight::{choose, read_value};
use crate::ddc::{DdcError, read_edid};
use crate::drm::{
    BusMatch, Connector, candidate_buses, connected, edid_name, i2c_dev_loaded, match_edid,
};
use crate::i2c::I2cOpener;
use crate::monitor::Link;

/// Por que se descobre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Why {
    /// Um método (`power.list`, ou um ajuste sem a tela): refaz as linhas
    /// com erro e espera a primeira leitura.
    Explicit,
    /// Um assinante novo do `power.screens`.
    Subscribed,
    /// O uevent do `drm` (ou eventos perdidos).
    Hotplug,
}

impl Screens {
    /// A integrada e os monitores ligados agora. Os monitores que ficam (o
    /// mesmo conector com o mesmo EDID) não são relidos; só os novos leem o
    /// EDID dos barramentos candidatos (fora os já tomados) e o brilho. Num
    /// pedido explícito ([`Why::Explicit`]), as linhas sem barramento
    /// (`i2c-dev` fora, acesso negado, não achado, EDID repetido) também
    /// tentam de novo, e a primeira leitura dos monitores novos é esperada;
    /// no hotplug, não (um monitor novo preso não para o laço).
    pub(super) async fn discover(&mut self, why: Why, events: &mut UnboundedReceiver<Event>) {
        self.discovered = true;
        self.discoveries.fetch_add(1, Ordering::Relaxed);
        self.discover_panel(why != Why::Hotplug);
        let fresh = self.discover_monitors(why == Why::Explicit).await;
        if why == Why::Explicit {
            // Cada leitura tem prazo: a espera termina.
            while self
                .monitors
                .values()
                .any(|m| m.reading && fresh.contains(&m.epoch))
            {
                match events.recv().await {
                    Some(e) => self.event(e),
                    None => break,
                }
            }
        }
        self.publish();
    }

    /// A integrada; com `reread`, o mesmo dispositivo também é relido (sem
    /// assinante não houve uevent: o valor pode ter mudado por fora).
    fn discover_panel(&mut self, reread: bool) {
        let roots = self.cfg.roots.clone();
        let Some(device) = choose(&roots) else {
            self.panel = None;
            return;
        };
        if let Some(p) = self
            .panel
            .as_mut()
            .filter(|p| p.device.device == device.device)
        {
            // O mesmo dispositivo: no hotplug o valor fica (as mudanças
            // chegam pelo uevent do backlight); o máximo é sempre conferido.
            let new_max = p.device.max != device.max;
            if new_max {
                p.adjuster.set_max(device.max);
            }
            p.device = device;
            if reread || new_max {
                self.reread_panel();
            }
            return;
        }
        let epoch = self.epoch();
        let (adjuster, shown, error) = match read_value(&roots, &device.device) {
            Ok(v) => (Adjuster::new(v, device.max), Some(v), None),
            Err(e) => (Adjuster::new(0, device.max), None, Some(read_failed(&e))),
        };
        self.panel = Some(Panel {
            epoch,
            device,
            adjuster,
            shown,
            error,
            writing: false,
            deferred: None,
            work: None,
        });
    }

    /// Os monitores: tira os que saíram (e, com `retry`, as linhas sem
    /// barramento), põe os novos e pede a primeira leitura deles. Devolve as
    /// gerações dos novos com barramento.
    async fn discover_monitors(&mut self, retry: bool) -> Vec<u64> {
        let roots = self.cfg.roots.clone();
        let Some(opener) = self.cfg.i2c.clone() else {
            // A raiz de teste: sem DDC.
            self.monitors.clear();
            return Vec::new();
        };
        let list = connected(&roots);
        let hashes: HashMap<String, u64> = list
            .iter()
            .map(|c| (c.name.clone(), edid_hash(&c.edid)))
            .collect();
        // Saiu ou trocou de monitor: a linha sai e a thread termina.
        self.monitors.retain(|name, m| {
            hashes.get(name) == Some(&m.edid_hash) && !(retry && m.link.is_none())
        });
        let new: Vec<Connector> = list
            .iter()
            .filter(|c| !self.monitors.contains_key(&c.name))
            .cloned()
            .collect();
        if new.is_empty() {
            return Vec::new();
        }
        if !i2c_dev_loaded(&roots) {
            for c in &new {
                let m = self.monitor(c, None, Err(NO_I2C_DEV.to_string()));
                self.monitors.insert(c.name.clone(), m);
            }
            return Vec::new();
        }

        let mut taken: Vec<u32> = list.iter().filter_map(|c| c.ddc).collect();
        taken.extend(self.monitors.values().filter_map(|m| m.bus));
        let mut buses: Vec<(Connector, Result<u32, String>)> = Vec::new();
        let mut by_card: BTreeMap<PathBuf, Vec<Connector>> = BTreeMap::new();
        for c in new {
            let key = (c.name.clone(), edid_hash(&c.edid));
            if let Some(bus) = c.ddc {
                buses.push((c, Ok(bus)));
            } else if let Some(bus) = self.cache.get(&key).filter(|b| !taken.contains(b)) {
                taken.push(*bus);
                buses.push((c, Ok(*bus)));
            } else {
                by_card.entry(c.pci.clone()).or_default().push(c);
            }
        }
        for (pci, conns) in by_card {
            let candidates = candidate_buses(&roots, &pci, &taken);
            let read = read_edids(&opener, &candidates, self.cfg.op_timeout).await;
            let pairs: Vec<(u32, Option<[u8; 128]>)> = read
                .iter()
                .map(|(bus, r)| (*bus, r.as_ref().ok().copied()))
                .collect();
            for c in conns {
                let found = match match_edid(&c.edid, &pairs) {
                    BusMatch::Found(bus) => {
                        self.cache.insert((c.name.clone(), edid_hash(&c.edid)), bus);
                        Ok(bus)
                    }
                    BusMatch::Ambiguous => Err(BUS_AMBIGUOUS.to_string()),
                    // Um candidato que não abriu pode ser o dele: o erro de
                    // abrir diz mais que "não achei".
                    BusMatch::NotFound => Err(open_error(&read)
                        .map(DdcError::message)
                        .unwrap_or_else(|| BUS_NOT_FOUND.to_string())),
                };
                buses.push((c, found));
            }
        }

        let mut fresh = Vec::new();
        for (c, bus) in buses {
            let link = bus
                .as_ref()
                .ok()
                .map(|b| Link::spawn(opener.clone(), *b, self.cfg.timing));
            let m = self.monitor(&c, link, bus);
            let epoch = m.epoch;
            let ready = m.link.is_some();
            self.monitors.insert(c.name.clone(), m);
            if ready {
                fresh.push(epoch);
                self.monitor_read(&c.name);
            }
        }
        fresh
    }

    fn monitor(&mut self, c: &Connector, link: Option<Link>, bus: Result<u32, String>) -> Monitor {
        let epoch = self.epoch();
        let (bus, error) = match bus {
            Ok(b) => (Some(b), None),
            Err(e) => (None, Some(e)),
        };
        Monitor {
            epoch,
            output: c.name.clone(),
            name: edid_name(&c.edid).unwrap_or_else(|| format!("Monitor {}", c.name)),
            edid_hash: edid_hash(&c.edid),
            bus,
            link,
            adjuster: None,
            max: None,
            shown: None,
            error,
            last_read: None,
            busy: false,
            stuck: false,
            reading: false,
            queued: Vec::new(),
            deferred: None,
            work: None,
        }
    }
}

fn edid_hash(edid: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    edid.hash(&mut h);
    h.finish()
}

/// O primeiro erro de abrir um barramento (sem acesso antes de sem o
/// `/dev/i2c-N`).
fn open_error(read: &[(u32, Result<[u8; 128], DdcError>)]) -> Option<&DdcError> {
    let errors = || read.iter().filter_map(|(_, r)| r.as_ref().err());
    errors()
        .find(|e| **e == DdcError::NoAccess)
        .or_else(|| errors().find(|e| matches!(e, DdcError::NoDevice(_))))
}

/// Lê o EDID de cada barramento, cada um numa thread solta com `limit`
/// (juntos: um barramento preso não segura os outros).
async fn read_edids(
    opener: &Arc<dyn I2cOpener>,
    buses: &[u32],
    limit: Duration,
) -> Vec<(u32, Result<[u8; 128], DdcError>)> {
    let tasks: Vec<_> = buses
        .iter()
        .map(|&bus| {
            let opener = opener.clone();
            tokio::spawn(async move {
                let read = move || {
                    let mut device = opener.open(bus).map_err(|e| DdcError::from_open(bus, &e))?;
                    read_edid(&mut *device)
                };
                match detached(read, limit).await {
                    Detached::Done(result) => result,
                    Detached::TimedOut => {
                        eprintln!("lucernad: o EDID do i2c-{bus} não veio em {limit:?}");
                        Err(DdcError::NoAnswer)
                    }
                    Detached::Panicked(p) => {
                        eprintln!(
                            "lucernad: a leitura do EDID do i2c-{bus} entrou em pânico: {:?}",
                            panic_message(&*p)
                        );
                        Err(DdcError::NoAnswer)
                    }
                    Detached::SpawnFailed(e) => {
                        eprintln!(
                            "lucernad: não consegui criar a thread do EDID do i2c-{bus}: {e}"
                        );
                        Err(DdcError::NoAnswer)
                    }
                }
            })
        })
        .collect();
    let mut out = Vec::with_capacity(tasks.len());
    for (bus, task) in buses.iter().zip(tasks) {
        out.push((*bus, task.await.unwrap_or(Err(DdcError::NoAnswer))));
    }
    out
}
