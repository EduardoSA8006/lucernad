//! Módulo `power` do `lucernad`: o brilho das telas, o histórico de consumo e
//! os perfis de energia.
//!
//! Toda leitura do sysfs e todo caminho de `/dev/i2c-N` saem de uma
//! [`roots::Roots`]: a de verdade (`/sys`, `/dev`) ou uma árvore falsa (os
//! testes, a fumaça e o `LUCERNAD_POWER_ROOT`). A escolha da tela integrada
//! fica no [`backlight`]; a descoberta dos monitores externos (os conectores,
//! os barramentos candidatos e o casamento pelo EDID), no [`drm`].

pub mod backlight;
pub mod drm;
pub mod roots;

#[cfg(feature = "testing")]
pub mod testing;
