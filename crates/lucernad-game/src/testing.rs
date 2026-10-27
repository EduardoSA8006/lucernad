//! Os dublês do modo jogo para os testes: o [`FakeProc`], um `/proc` falso
//! numa pasta temporária.

use std::path::PathBuf;

use tempfile::TempDir;

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
