//! O juntar de ajustes de uma tela, como máquina de estado pura.
//!
//! No máximo uma escrita em voo: um pedido no meio só troca o alvo, e o
//! `change` soma ao alvo pendente (ou ao em voo, ou ao atual), de modo que
//! segurar a tecla num monitor DDC lento soma tudo. Depois da última escrita,
//! a releitura é publicada; as releituras do meio, não (o slider não pula
//! para trás).
//!
//! Unidades: [`Adjuster::set`] e [`Adjuster::change`] recebem **percentual**;
//! [`Step::Write`], [`Step::Publish`], [`Adjuster::new`],
//! [`Adjuster::written`] e [`Adjuster::external`] usam o valor **bruto** da
//! tela (de 0 ao `max` dela).

/// O valor bruto de um percentual: `max(1, round(p × max / 100))`. O mínimo
/// é 1 (uma tela nunca é apagada por um ajuste).
pub fn raw_of(percent: u8, max: u32) -> u32 {
    let percent = u64::from(percent.min(100));
    let raw = (percent * u64::from(max) + 50) / 100;
    // `raw` ≤ `max`, então cabe num `u32`.
    u32::try_from(raw).unwrap_or(max).max(1)
}

/// O percentual (arredondado, 0–100) de um valor bruto; 0 sem máximo.
pub fn percent_of(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    let raw = u64::from(raw.min(max));
    let percent = (raw * 100 + u64::from(max) / 2) / u64::from(max);
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// O que o laço faz depois de um passo do [`Adjuster`].
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Escrever este valor bruto na tela (e chamar `written` com a releitura).
    Write(u32),
    /// Publicar este valor bruto (a releitura depois da última escrita).
    Publish(u32),
    /// A escrita falhou com este erro; nada mais pendente.
    Failed(String),
    /// Nada a fazer agora (uma escrita já está em voo).
    Idle,
}

/// O juntar de ajustes de uma tela (veja o módulo).
#[derive(Debug)]
pub struct Adjuster {
    current: u32,
    max: u32,
    in_flight: Option<u32>,
    pending: Option<u32>,
}

impl Adjuster {
    /// Uma tela com o valor bruto `current` de `max`, sem nada em voo.
    pub fn new(current: u32, max: u32) -> Adjuster {
        Adjuster {
            current,
            max,
            in_flight: None,
            pending: None,
        }
    }

    /// Pede o brilho `percent` (1–100): escreve já se nada está em voo;
    /// senão só troca o alvo pendente.
    pub fn set(&mut self, percent: u8) -> Step {
        let target = raw_of(percent.clamp(1, 100), self.max);
        self.request(target)
    }

    /// Soma `delta` pontos percentuais ao alvo pendente (ou ao em voo, ou ao
    /// atual), com o resultado limitado a 1–100.
    pub fn change(&mut self, delta: i32) -> Step {
        let base = self.pending.or(self.in_flight).unwrap_or(self.current);
        let percent = i32::from(percent_of(base, self.max)).saturating_add(delta);
        let percent = u8::try_from(percent.clamp(1, 100)).unwrap_or(100);
        let floor = raw_of(1, self.max);
        let mut target = raw_of(percent, self.max);
        // Com um máximo pequeno, um passo de percentual pode cair no mesmo
        // valor bruto: anda um, para a tecla nunca ficar sem efeito. Só longe
        // dos limites (1% e 100%): no limite o valor fica nele, e o resultado
        // nunca sai de [1%, max] (uma base abaixo de 1% com delta negativo vai
        // ao piso, e o "anda um" nunca a leva além dele).
        if target == base && delta != 0 && percent > 1 && percent < 100 {
            target = if delta > 0 {
                base.saturating_add(1)
            } else {
                base.saturating_sub(1)
            };
        }
        let target = target.clamp(floor, self.max.max(floor));
        self.request(target)
    }

    fn request(&mut self, target: u32) -> Step {
        if self.in_flight.is_some() {
            self.pending = Some(target);
            Step::Idle
        } else {
            self.in_flight = Some(target);
            Step::Write(target)
        }
    }

    /// A escrita em voo terminou com esta releitura (ou erro). Com um alvo
    /// pendente diferente do lido, escreve o alvo; sem alvo, publica o lido
    /// ou diz o erro. Um erro com alvo pendente ainda tenta o alvo (o último
    /// pedido vale; se falhar de novo, o erro chega no próximo `written`).
    pub fn written(&mut self, read: Result<u32, String>) -> Step {
        self.in_flight = None;
        let pending = self.pending.take();
        match read {
            Ok(value) => {
                self.current = value;
                match pending {
                    Some(target) if target != value => {
                        self.in_flight = Some(target);
                        Step::Write(target)
                    }
                    _ => Step::Publish(value),
                }
            }
            Err(error) => match pending {
                Some(target) => {
                    self.in_flight = Some(target);
                    Step::Write(target)
                }
                None => Step::Failed(error),
            },
        }
    }

    /// Uma mudança de fora (o uevent do backlight, uma releitura): guarda o
    /// valor e diz se publica (só sem nada em voo nem pendente).
    pub fn external(&mut self, read: u32) -> bool {
        self.current = read;
        !self.busy()
    }

    /// Há uma escrita em voo (ou pendente).
    pub fn busy(&self) -> bool {
        self.in_flight.is_some() || self.pending.is_some()
    }

    /// O último valor bruto conhecido da tela.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Troca o máximo (a releitura do DDC pode trazer outro). Se o máximo
    /// mudou, o alvo pendente e o em voo são descartados: estão em valor bruto
    /// do máximo antigo. A escrita que já saiu ainda chama `written`, que só
    /// publica a releitura.
    pub fn set_max(&mut self, max: u32) {
        if max != self.max {
            self.pending = None;
            self.in_flight = None;
        }
        self.max = max;
    }
}
