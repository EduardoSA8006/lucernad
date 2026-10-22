//! A sonda manual do i2c, **só leitura**: `sonda_i2c <barramento>` lê o EDID
//! (0x50) e o brilho (Get VCP 0x10) do monitor no `/dev/i2c-<barramento>`.
//!
//! Recusa (código 2), sem abrir nada, o barramento que não seja de placa de
//! vídeo. Não grava nenhum VCP: nada aqui muda o brilho. Nunca roda em teste
//! automatizado; quem roda é o agente, na máquina do usuário, avisado antes.

use std::process::ExitCode;
use std::time::Instant;

use lucernad_power::ddc::{DdcError, DdcTiming, get_brightness, read_edid};
use lucernad_power::drm::{edid_name, is_gpu_bus};
use lucernad_power::i2c::{I2cOpener, LinuxI2c};
use lucernad_power::roots::Roots;

fn main() -> ExitCode {
    let Some(bus) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
        eprintln!("uso: sonda_i2c <barramento>");
        return ExitCode::from(2);
    };
    let roots = Roots::system();
    if !is_gpu_bus(&roots, bus) {
        eprintln!("i2c-{bus}: não é barramento de placa de vídeo; recusado.");
        return ExitCode::from(2);
    }

    let start = Instant::now();
    let opener = LinuxI2c {
        dev: roots.dev.clone(),
    };
    let mut device = match opener.open(bus) {
        Ok(device) => device,
        Err(e) => {
            println!(
                "i2c-{bus}: não abriu: {e} ({:?})",
                DdcError::from_open(bus, &e)
            );
            return ExitCode::FAILURE;
        }
    };
    println!("i2c-{bus}: aberto em {:?}", start.elapsed());

    let start = Instant::now();
    match read_edid(device.as_mut()) {
        Ok(edid) => {
            let head: Vec<String> = edid[..8].iter().map(|b| format!("{b:02x}")).collect();
            println!(
                "EDID ({:?}): {}  nome: {:?}",
                start.elapsed(),
                head.join(" "),
                edid_name(&edid)
            );
            let hex: Vec<String> = edid.iter().map(|b| format!("{b:02x}")).collect();
            println!("EDID completo: {}", hex.concat());
        }
        Err(e) => println!("EDID ({:?}): erro {e:?}", start.elapsed()),
    }

    let start = Instant::now();
    match get_brightness(device.as_mut(), &DdcTiming::standard()) {
        Ok(vcp) => println!(
            "brilho ({:?}): atual {} de máximo {}",
            start.elapsed(),
            vcp.current,
            vcp.max
        ),
        Err(e) => println!("brilho ({:?}): erro {e:?}", start.elapsed()),
    }
    ExitCode::SUCCESS
}
