//! A sonda dos barramentos de verdade, ignorada por padrão: só à mão, numa
//! sessão de verdade (`cargo test -p lucernad-game --test sonda -- --ignored`).
//! Segura e solta o perfil de desempenho e pergunta pelo GameMode, sem
//! deixar nada preso.

use lucernad_game::bus::{BusError, GameBus, ZbusGame};

#[tokio::test]
#[ignore = "fala com o D-Bus de verdade"]
async fn sonda_real_buses() {
    let bus = ZbusGame::system();
    match bus.hold_performance().await {
        Ok(cookie) => {
            println!("perfil de desempenho segurado (cookie {cookie})");
            bus.release(cookie).await.unwrap();
            println!("solto");
        }
        Err(BusError::Absent) => println!("sem o serviço de perfis"),
        Err(e) => panic!("perfil: {e}"),
    }
    println!("GameMode presente: {:?}", bus.gamemode_present().await);
}
