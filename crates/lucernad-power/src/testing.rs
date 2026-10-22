//! Ajudantes para os testes (feature `testing`): o montador de árvores falsas
//! do sysfs e um EDID de exemplo.
//!
//! Nada aqui toca o `/sys` ou o `/dev` de verdade: tudo fica sob a pasta dada
//! ao [`Tree::new`].

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::roots::Roots;

/// Monta uma árvore falsa do sysfs sobre um diretório, com os links
/// simbólicos de verdade (`class/drm/<card>` → `devices/<pci>/drm/<card>`,
/// `class/i2c-dev/i2c-N/device` → o adaptador, o `ddc` dos conectores).
pub struct Tree {
    base: PathBuf,
}

impl Tree {
    /// Uma árvore vazia (só `sys` e `dev`) em `dir`.
    pub fn new(dir: &Path) -> Tree {
        let tree = Tree {
            base: dir.to_path_buf(),
        };
        mkdir(&tree.sys(""));
        mkdir(&tree.base.join("dev"));
        tree
    }

    /// A raiz desta árvore.
    pub fn roots(&self) -> Roots {
        Roots::at(&self.base)
    }

    /// Um backlight em `class/backlight/<device>`, com o `device` apontando
    /// para o conector `connector` (`card2-eDP-1`) da placa dele, se dado.
    pub fn backlight(
        &self,
        device: &str,
        kind: &str,
        max: u32,
        value: u32,
        actual: u32,
        connector: Option<&str>,
    ) {
        let dir = self.sys("class/backlight").join(device);
        mkdir(&dir);
        write(&dir.join("type"), kind);
        write(&dir.join("max_brightness"), &max.to_string());
        write(&dir.join("brightness"), &value.to_string());
        write(&dir.join("actual_brightness"), &actual.to_string());
        write(&dir.join("bl_power"), "0");
        if let Some(connector) = connector {
            let class = self.sys("class/drm").join(connector);
            let target = class.canonicalize().unwrap_or(class);
            link(&target, &dir.join("device"));
        }
    }

    /// Um dispositivo em `devices/<path>`, com o `class` se dado.
    pub fn device(&self, path: &str, class: Option<&str>) {
        let dir = self.sys("devices").join(path);
        mkdir(&dir);
        if let Some(class) = class {
            write(&dir.join("class"), class);
        }
    }

    /// Uma placa de vídeo: `devices/<pci>` com o `class`, a `drm/<card>` sob
    /// ela (com o `device` de volta) e o `class/drm/<card>`.
    pub fn gpu(&self, pci: &str, class: &str, card: &str) {
        self.device(pci, Some(class));
        let pci_dir = self.sys("devices").join(pci);
        let card_dir = pci_dir.join("drm").join(card);
        mkdir(&card_dir);
        link(&pci_dir, &card_dir.join("device"));
        link(&card_dir, &self.sys("class/drm").join(card));
    }

    /// Um conector `<card>-<name>` da placa `card` (criada antes pelo
    /// [`Tree::gpu`]), com o `status` e o `edid`; com `ddc`, o barramento
    /// AUX `i2c-<n>` sob o conector e o link `ddc` para ele. Chamado de novo,
    /// troca o que já havia.
    pub fn connector(&self, card: &str, name: &str, status: &str, edid: &[u8], ddc: Option<u32>) {
        let card_dir = self
            .sys("class/drm")
            .join(card)
            .canonicalize()
            .expect("a placa do conector (Tree::gpu antes)");
        let entry = format!("{card}-{name}");
        let dir = card_dir.join(&entry);
        mkdir(&dir);
        write(&dir.join("status"), status);
        std::fs::write(dir.join("edid"), edid).expect("escrever o edid");
        link(&dir, &self.sys("class/drm").join(&entry));
        if let Some(n) = ddc {
            let adapter = self.adapter(n, &dir);
            link(&adapter, &dir.join("ddc"));
        }
    }

    /// Um barramento `i2c-<n>` com o `i2c-dev` sob `devices/<parent>`.
    pub fn bus(&self, n: u32, parent: &str) {
        let parent = self.sys("devices").join(parent);
        self.adapter(n, &parent);
    }

    /// Troca o `brightness` do backlight `device`.
    pub fn set_value(&self, device: &str, value: u32) {
        let path = self.brightness(device);
        write(&path, &value.to_string());
    }

    /// O `brightness` do backlight `device`.
    pub fn value(&self, device: &str) -> u32 {
        let path = self.brightness(device);
        let text = std::fs::read_to_string(&path).expect("ler o brightness");
        text.trim().parse().expect("o brightness é um número")
    }

    fn sys(&self, rel: &str) -> PathBuf {
        self.base.join("sys").join(rel)
    }

    fn brightness(&self, device: &str) -> PathBuf {
        self.sys("class/backlight").join(device).join("brightness")
    }

    /// O adaptador `i2c-<n>` sob `parent`, com o `i2c-dev/i2c-<n>` (e o
    /// `device` de volta) e o `class/i2c-dev/i2c-<n>`. Devolve o adaptador.
    fn adapter(&self, n: u32, parent: &Path) -> PathBuf {
        let bus = format!("i2c-{n}");
        let adapter = parent.join(&bus);
        let dev = adapter.join("i2c-dev").join(&bus);
        mkdir(&dev);
        write(&adapter.join("name"), &format!("adaptador falso {n}"));
        write(&dev.join("dev"), &format!("89:{n}"));
        link(&adapter, &dev.join("device"));
        link(&dev, &self.sys("class/i2c-dev").join(&bus));
        adapter
    }
}

/// 128 bytes de um EDID 1.4 válido, com o `serial` no cabeçalho, o nome no
/// descritor 0xFC (cortado em 13 bytes) e a soma de verificação certa.
pub fn edid(name: &str, serial: u32) -> Vec<u8> {
    let mut edid = vec![0u8; 128];
    edid[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    edid[8..10].copy_from_slice(&[0x1E, 0x6D]); // "GSM"
    edid[10..12].copy_from_slice(&[0x01, 0x77]);
    edid[12..16].copy_from_slice(&serial.to_le_bytes());
    edid[16] = 1; // semana
    edid[17] = 32; // 2022
    edid[18] = 1;
    edid[19] = 4;
    // Descritor 0: um modo de exemplo (pixel clock diferente de zero).
    edid[54..56].copy_from_slice(&[0x02, 0x3A]);
    // Descritor 1: o número de série em texto (0xFF).
    descriptor(&mut edid[72..90], 0xFF, &serial.to_string());
    // Descritor 2: o nome (0xFC).
    descriptor(&mut edid[90..108], 0xFC, name);
    // Descritor 3: vazio (0x10).
    edid[111] = 0x10;
    let sum = edid[..127].iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    edid[127] = 0u8.wrapping_sub(sum);
    edid
}

/// Um descritor de texto: `00 00 00 <tag> 00`, até 13 bytes, `0x0A` e espaços.
fn descriptor(slot: &mut [u8], tag: u8, text: &str) {
    slot[..5].copy_from_slice(&[0, 0, 0, tag, 0]);
    let text = &text.as_bytes()[..text.len().min(13)];
    slot[5..5 + text.len()].copy_from_slice(text);
    if text.len() < 13 {
        slot[5 + text.len()] = 0x0A;
        slot[6 + text.len()..].fill(0x20);
    }
}

fn mkdir(dir: &Path) {
    std::fs::create_dir_all(dir).expect("criar a pasta da árvore falsa");
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, format!("{text}\n")).expect("escrever na árvore falsa");
}

/// Um link simbólico em `at` para `target`, trocando o que houver.
fn link(target: &Path, at: &Path) {
    if let Some(parent) = at.parent() {
        mkdir(parent);
    }
    if at.symlink_metadata().is_ok() {
        std::fs::remove_file(at).expect("trocar o link da árvore falsa");
    }
    symlink(target, at).expect("criar o link da árvore falsa");
}
