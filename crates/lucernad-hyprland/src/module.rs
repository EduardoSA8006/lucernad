//! O módulo: o tópico `hyprland.status` (`{available, instance, version,
//! error}`), relido a cada conexão e queda dos eventos e, indisponível, a
//! cada [`RECHECK`].

use std::sync::Arc;
use std::time::Duration;

use lucerna_service::{ServerBuilder, Topic};
use lucernad_core::{ModuleTask, publish_changed, supervise};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::events::{CONNECTED, DISCONNECTED};
use crate::{Hypr, SocketHypr, version};

/// Indisponível, de quanto em quanto tempo o estado é relido.
pub const RECHECK: Duration = Duration::from_secs(5);

const PANIC_ERROR: &str = "O módulo do Hyprland parou por um erro interno.";

/// De onde o módulo fala com o Hyprland.
pub struct HyprlandConfig {
    pub hypr: Arc<dyn Hypr>,
}

impl HyprlandConfig {
    /// O Hyprland de verdade, pelo ambiente.
    pub fn system() -> HyprlandConfig {
        HyprlandConfig {
            hypr: Arc::new(SocketHypr::discover()),
        }
    }
}

/// Os tópicos do módulo.
#[derive(Clone)]
pub struct HyprlandTopics {
    /// O `hyprland.status`: `{available, instance, version, error}`.
    pub status: Topic,
}

/// O módulo registrado, pronto para rodar.
pub struct Hyprland {
    pub topics: HyprlandTopics,
    hypr: Arc<dyn Hypr>,
}

/// Declara o tópico `hyprland.status`.
pub fn register(server: &mut ServerBuilder, cfg: HyprlandConfig) -> Hyprland {
    let status = server.topic(
        "hyprland.status",
        json!({"available": false, "instance": null, "version": null,
               "error": "Procurando o Hyprland."}),
    );
    Hyprland {
        topics: HyprlandTopics { status },
        hypr: cfg.hypr,
    }
}

impl Hyprland {
    /// O Hyprland do módulo, para os outros módulos (o `game`).
    pub fn hypr(&self) -> Arc<dyn Hypr> {
        self.hypr.clone()
    }

    /// Roda o laço do estado e o supervisor (chame dentro do runtime).
    pub fn spawn(self) -> ModuleTask {
        let status = self.topics.status.clone();
        let run = tokio::spawn(run(self.hypr, self.topics.status));
        supervise(
            run,
            status,
            "do Hyprland",
            json!({"available": false, "instance": null, "version": null, "error": PANIC_ERROR}),
        )
    }
}

/// Uma frase: primeira letra maiúscula e ponto final.
fn sentence(text: &str) -> String {
    let text = text.trim().trim_end_matches('.');
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => format!("{}{}.", c.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

async fn check(hypr: &dyn Hypr, status: &Topic) -> bool {
    let data: Value = match version(hypr).await {
        Ok(v) => {
            json!({"available": true, "instance": hypr.instance(), "version": v, "error": null})
        }
        Err(e) => json!({"available": false, "instance": null, "version": null,
                         "error": sentence(&e.to_string())}),
    };
    let ok = data["available"] == json!(true);
    publish_changed(status, data);
    ok
}

async fn run(hypr: Arc<dyn Hypr>, status: Topic) {
    let mut events = hypr.events();
    let mut ok = check(&*hypr, &status).await;
    loop {
        tokio::select! {
            e = events.recv() => match e {
                Ok(e) if e.name == CONNECTED || e.name == DISCONNECTED => {
                    ok = check(&*hypr, &status).await;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => std::future::pending::<()>().await,
            },
            _ = tokio::time::sleep(RECHECK), if !ok => {
                ok = check(&*hypr, &status).await;
            }
        }
    }
}
