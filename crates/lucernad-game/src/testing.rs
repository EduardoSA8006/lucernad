//! Os dublês do modo jogo para os testes: o [`FakeProc`], um `/proc` falso
//! numa pasta temporária, e o [`FakeGameBus`], o D-Bus falso do perfil e do
//! GameMode.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::bus::{BoxFuture, BusError, GameBus};
use crate::detect::ProcRoot;

/// Um `/proc` falso numa pasta temporária (apagada ao largar).
pub struct FakeProc {
    dir: TempDir,
    uid: u32,
}

impl FakeProc {
    /// Um `/proc` vazio, visto pelo usuário `uid`.
    pub fn new(uid: u32) -> FakeProc {
        FakeProc {
            dir: tempfile::tempdir().unwrap(),
            uid,
        }
    }

    /// A pasta do processo `pid`.
    pub fn path(&self, pid: i32) -> PathBuf {
        self.dir.path().join(pid.to_string())
    }

    /// Um processo: o `status` (`PPid` e `Uid`), o `comm`, o link `exe`, o
    /// `cmdline` e o `environ` (separados por `\0`).
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &self,
        pid: i32,
        ppid: i32,
        uid: u32,
        comm: &str,
        exe: &str,
        cmdline: &[&str],
        env: &[(&str, &str)],
    ) {
        let dir = self.path(pid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("status"),
            format!(
                "Name:\t{comm}\nPid:\t{pid}\nPPid:\t{ppid}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        let _ = std::fs::remove_file(dir.join("exe"));
        std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        let mut cmd = cmdline.join("\0");
        cmd.push('\0');
        std::fs::write(dir.join("cmdline"), cmd).unwrap();
        let environ: String = env.iter().map(|(k, v)| format!("{k}={v}\0")).collect();
        std::fs::write(dir.join("environ"), environ).unwrap();
    }

    /// A raiz para a detecção.
    pub fn root(&self) -> ProcRoot {
        ProcRoot::at(self.dir.path(), self.uid)
    }
}

/// O D-Bus falso do modo jogo: o perfil segurado, os pids registrados no
/// GameMode, as chamadas gravadas e as falhas roteirizadas.
pub struct FakeGameBus {
    state: std::sync::Mutex<FakeBusState>,
}

#[derive(Default)]
struct FakeBusState {
    no_profiles: bool,
    no_gamemode: bool,
    hang: bool,
    fail_next: Vec<BusError>,
    calls: Vec<String>,
    held: Option<u32>,
    next_cookie: u32,
    registered: Vec<i32>,
}

impl Default for FakeGameBus {
    fn default() -> FakeGameBus {
        FakeGameBus::new()
    }
}

impl FakeGameBus {
    /// Com o serviço de perfis e o GameMode no barramento.
    pub fn new() -> FakeGameBus {
        FakeGameBus {
            state: std::sync::Mutex::new(FakeBusState {
                next_cookie: 7,
                ..FakeBusState::default()
            }),
        }
    }

    /// Sem o serviço de perfis.
    pub fn absent_profiles(&self) {
        self.state.lock().unwrap().no_profiles = true;
    }

    /// Sem o GameMode.
    pub fn absent_gamemode(&self) {
        self.state.lock().unwrap().no_gamemode = true;
    }

    /// A próxima chamada falha com `e`.
    pub fn fail_next(&self, e: BusError) {
        self.state.lock().unwrap().fail_next.push(e);
    }

    /// Toda chamada fica sem resposta.
    pub fn hang(&self) {
        self.state.lock().unwrap().hang = true;
    }

    /// As chamadas, na ordem (`hold`, `release N`, `register P`,
    /// `unregister P`).
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    /// O cookie segurado agora.
    pub fn held(&self) -> Option<u32> {
        self.state.lock().unwrap().held
    }

    /// Os pids registrados agora, na ordem.
    pub fn registered(&self) -> Vec<i32> {
        self.state.lock().unwrap().registered.clone()
    }

    async fn enter(&self, call: String) -> Result<(), BusError> {
        let hang = {
            let mut s = self.state.lock().unwrap();
            if !s.hang {
                s.calls.push(call);
                if !s.fail_next.is_empty() {
                    return Err(s.fail_next.remove(0));
                }
            }
            s.hang
        };
        if hang {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

impl GameBus for FakeGameBus {
    fn hold_performance(&self) -> BoxFuture<'_, Result<u32, BusError>> {
        Box::pin(async move {
            self.enter("hold".into()).await?;
            let mut s = self.state.lock().unwrap();
            if s.no_profiles {
                return Err(BusError::Absent);
            }
            let c = s.next_cookie;
            s.next_cookie += 1;
            s.held = Some(c);
            Ok(c)
        })
    }

    fn release(&self, cookie: u32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            self.enter(format!("release {cookie}")).await?;
            let mut s = self.state.lock().unwrap();
            if s.held == Some(cookie) {
                s.held = None;
            }
            Ok(())
        })
    }

    fn gamemode_present(&self) -> BoxFuture<'_, Result<bool, BusError>> {
        Box::pin(async move { Ok(!self.state.lock().unwrap().no_gamemode) })
    }

    fn register_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            self.enter(format!("register {pid}")).await?;
            let mut s = self.state.lock().unwrap();
            if s.no_gamemode {
                return Err(BusError::Absent);
            }
            s.registered.push(pid);
            Ok(())
        })
    }

    fn unregister_game(&self, pid: i32) -> BoxFuture<'_, Result<(), BusError>> {
        Box::pin(async move {
            self.enter(format!("unregister {pid}")).await?;
            let mut s = self.state.lock().unwrap();
            if s.no_gamemode {
                return Err(BusError::Absent);
            }
            s.registered.retain(|p| *p != pid);
            Ok(())
        })
    }
}
