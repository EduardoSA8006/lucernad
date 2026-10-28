//! A decisão de entrar e sair (D8 e D9 do spec), sem E/S: os gatilhos de
//! cada janela e a máquina com a trava manual e as esperas.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use lucernad_hyprland::clients::Window;

use crate::config::GameSettings;

/// A tela cheia só conta depois disso contínuo.
pub const FULLSCREEN_ENTER: Duration = Duration::from_secs(1);
/// Sem nenhum gatilho por isso, o modo automático sai.
pub const AUTO_EXIT: Duration = Duration::from_secs(3);

/// Por que uma janela liga o modo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    Fullscreen,
    Game,
    Always,
}

impl Reason {
    /// O nome no `game.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Fullscreen => "fullscreen",
            Reason::Game => "game",
            Reason::Always => "always",
        }
    }
}

/// Uma janela que liga o modo, e por quê.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub window: Window,
    pub reason: Reason,
}

/// Os gatilhos das janelas: a lista "nunca" tira a janela de tudo; a
/// "sempre" liga; depois o jogo detectado (`is_game` do pid, só com
/// `auto.games`) e a tela cheia de verdade (o bit 2 do `fullscreen` num
/// workspace à mostra, só com `auto.fullscreen`). Uma razão por janela, e só
/// janelas mapeadas.
pub fn triggers(
    windows: &[Window],
    active_ws: &[i64],
    mut is_game: impl FnMut(i32) -> bool,
    s: &GameSettings,
) -> Vec<Trigger> {
    let mut out = Vec::new();
    for w in windows.iter().filter(|w| w.mapped) {
        let class = w.class.to_lowercase();
        if s.never.contains(&class) {
            continue;
        }
        let reason = if s.always.contains(&class) {
            Some(Reason::Always)
        } else if s.auto_games && is_game(w.pid) {
            Some(Reason::Game)
        } else if s.auto_fullscreen && w.fullscreen & 2 != 0 && active_ws.contains(&w.workspace) {
            Some(Reason::Fullscreen)
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(Trigger {
                window: w.clone(),
                reason,
            });
        }
    }
    out
}

/// O que a máquina mostra.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub active: bool,
    /// `Some(true)` = ligado na mão (só sai na mão); `Some(false)` = preso
    /// desligado até os gatilhos sumirem.
    pub manual: Option<bool>,
    /// `manual`, `fullscreen`, `game`, `always`, nessa ordem.
    pub reasons: Vec<&'static str>,
    /// Os gatilhos que contam agora.
    pub windows: Vec<Trigger>,
    pub since: Option<SystemTime>,
}

/// A máquina do modo jogo (D9): gatilhos, trava manual e as esperas.
pub struct Machine {
    enter: Duration,
    exit: Duration,
    manual: Option<bool>,
    active: bool,
    since: Option<SystemTime>,
    current: Vec<Trigger>,
    first_seen: HashMap<String, Instant>,
    effective: Vec<Trigger>,
    /// A última vez em que havia gatilho (enquanto a espera da saída corre).
    last_effective: Option<Instant>,
    last_reasons: Vec<Reason>,
}

impl Default for Machine {
    fn default() -> Machine {
        Machine::new()
    }
}

impl Machine {
    /// Com as esperas [`FULLSCREEN_ENTER`] e [`AUTO_EXIT`].
    pub fn new() -> Machine {
        Machine::with_timing(FULLSCREEN_ENTER, AUTO_EXIT)
    }

    /// Com outras esperas (os testes do laço).
    pub fn with_timing(enter: Duration, exit: Duration) -> Machine {
        Machine {
            enter,
            exit,
            manual: None,
            active: false,
            since: None,
            current: Vec::new(),
            first_seen: HashMap::new(),
            effective: Vec::new(),
            last_effective: None,
            last_reasons: Vec::new(),
        }
    }

    /// Os gatilhos de agora.
    pub fn update(&mut self, triggers: Vec<Trigger>, now: Instant) {
        if !self.effective.is_empty() {
            self.last_effective = Some(now);
        }
        self.first_seen.retain(|addr, _| {
            triggers
                .iter()
                .any(|t| t.reason == Reason::Fullscreen && &t.window.address == addr)
        });
        for t in triggers.iter().filter(|t| t.reason == Reason::Fullscreen) {
            self.first_seen
                .entry(t.window.address.clone())
                .or_insert(now);
        }
        self.current = triggers;
        self.recompute(now);
    }

    /// Liga ou desliga na mão.
    pub fn set_manual(&mut self, on: bool, now: Instant) {
        self.recompute(now);
        self.manual = if on {
            Some(true)
        } else if self.auto_wants(now) {
            Some(false)
        } else {
            None
        };
        self.recompute(now);
    }

    /// O próximo instante em que a decisão pode mudar sem gatilho novo.
    pub fn next_deadline(&self) -> Option<Instant> {
        let enter = self
            .current
            .iter()
            .filter(|t| t.reason == Reason::Fullscreen)
            .filter(|t| {
                !self
                    .effective
                    .iter()
                    .any(|e| e.window.address == t.window.address)
            })
            .filter_map(|t| {
                self.first_seen
                    .get(&t.window.address)
                    .map(|f| *f + self.enter)
            });
        let exit = self
            .effective
            .is_empty()
            .then(|| self.last_effective.map(|l| l + self.exit))
            .flatten();
        enter.chain(exit).min()
    }

    /// Reavalia as esperas em `now`.
    pub fn tick(&mut self, now: Instant) {
        self.recompute(now);
    }

    /// Há gatilho agora, ou a espera da saída ainda corre.
    fn auto_wants(&self, now: Instant) -> bool {
        !self.effective.is_empty() || self.last_effective.is_some_and(|l| now < l + self.exit)
    }

    fn recompute(&mut self, now: Instant) {
        self.effective = self
            .current
            .iter()
            .filter(|t| {
                t.reason != Reason::Fullscreen
                    || self
                        .first_seen
                        .get(&t.window.address)
                        .is_some_and(|f| now >= *f + self.enter)
            })
            .cloned()
            .collect();
        if !self.effective.is_empty() {
            self.last_effective = Some(now);
            let mut r: Vec<Reason> = self.effective.iter().map(|t| t.reason).collect();
            r.sort();
            r.dedup();
            self.last_reasons = r;
        }
        let auto = self.auto_wants(now);
        if !auto {
            self.last_effective = None;
            self.last_reasons.clear();
        }
        self.active = match self.manual {
            Some(true) => true,
            Some(false) => {
                if !auto {
                    self.manual = None;
                }
                false
            }
            None => auto,
        };
        if !self.active {
            self.since = None;
        } else if self.since.is_none() {
            self.since = Some(SystemTime::now());
        }
    }

    /// O estado agora.
    pub fn view(&self) -> View {
        let mut reasons = Vec::new();
        if self.manual == Some(true) {
            reasons.push("manual");
        }
        if self.active {
            reasons.extend(self.last_reasons.iter().map(|r| r.as_str()));
        }
        View {
            active: self.active,
            manual: self.manual,
            reasons,
            windows: self.effective.clone(),
            since: self.since,
        }
    }
}
