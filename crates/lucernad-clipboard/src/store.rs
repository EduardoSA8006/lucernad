//! O histórico: SQLite com busca trigram e as imagens em arquivos.
//!
//! O trabalho pesado de cada cópia (hash, normalização, miniatura e gravação
//! dos arquivos) fica no [`prepare`], que não precisa do [`Store`]; o
//! [`Store::insert`] só faz a transação. Assim o lock do `Store` nunca espera
//! uma imagem grande.

use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lucerna_paths::create_private_dir;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::mime::{Kind, uri_lines};
use crate::search::{SHORT_QUERY, file_names, normalize, preview_for};
use crate::thumb::{extension_for, thumbnail};

/// O arquivo do banco no disco.
const DB_FILE: &str = "history.db";
/// A pasta das imagens e das miniaturas.
const IMAGES_DIR: &str = "images";
/// A versão do esquema.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS items(
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    hash TEXT NOT NULL UNIQUE,
    text TEXT,
    norm TEXT,
    mime TEXT,
    width INT,
    height INT,
    size INT NOT NULL,
    lines INT,
    file TEXT,
    created INT NOT NULL,
    used INT NOT NULL,
    pinned INT NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS items_order ON items(pinned DESC, used DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts
    USING fts5(norm, content='items', content_rowid='id', tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
    INSERT INTO items_fts(rowid, norm) VALUES (new.id, new.norm);
END;
CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, norm) VALUES ('delete', old.id, old.norm);
END;
CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE OF norm ON items BEGIN
    INSERT INTO items_fts(items_fts, rowid, norm) VALUES ('delete', old.id, old.norm);
    INSERT INTO items_fts(rowid, norm) VALUES (new.id, new.norm);
END;
";

const COLUMNS: &str =
    "id, kind, hash, text, mime, width, height, size, lines, file, created, used, pinned";

fn err(e: impl Display) -> String {
    e.to_string()
}

/// Uma cópia nova, como chegou da área de transferência.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewClip {
    Text(String),
    /// A `text/uri-list` crua.
    Files(String),
    Image {
        mime: String,
        bytes: Vec<u8>,
    },
}

/// Onde o histórico mora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A pasta do banco no disco (`history.db` e `images/`).
    Disk(PathBuf),
    /// Sem persistência: o banco em memória, os arquivos em `files/images/`,
    /// e `disk`, a pasta do disco, que é apagada.
    Memory { files: PathBuf, disk: PathBuf },
}

/// Uma cópia já preparada para o [`Store::insert`].
#[derive(Debug)]
pub struct Prepared {
    kind: Kind,
    hash: String,
    text: Option<String>,
    norm: Option<String>,
    mime: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    size: usize,
    lines: Option<usize>,
    /// O nome do arquivo da imagem, dentro de `images/`.
    file: Option<String>,
    /// Os bytes da imagem e da miniatura, para regravar o que faltar.
    image: Option<(Vec<u8>, Option<Vec<u8>>)>,
}

/// O hash do conteúdo, com o tipo na frente: SHA-256 em hex.
fn hash_of(kind: Kind, content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(content);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn thumb_name(hash: &str) -> String {
    format!("{hash}.thumb.png")
}

/// Grava `bytes` em `path` por um arquivo temporário na mesma pasta e um
/// `rename`; se `path` já existe, não faz nada.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map_err(|e| format!("{}: {e}", path.display()))
}

fn remove_quietly(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn remove_dir_quietly(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn private_dir(dir: &Path) -> Result<(), String> {
    create_private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Os arquivos das imagens ficam em `files_dir/images/`.
fn images_in(files_dir: &Path) -> PathBuf {
    files_dir.join(IMAGES_DIR)
}

/// Grava os arquivos da imagem que ainda não existirem em `images`.
fn write_image_files(images: &Path, p: &Prepared) -> Result<(), String> {
    let (Some(file), Some((bytes, thumb))) = (&p.file, &p.image) else {
        return Ok(());
    };
    private_dir(images)?;
    write_new(&images.join(file), bytes)?;
    if let Some(thumb) = thumb {
        write_new(&images.join(thumb_name(&p.hash)), thumb)?;
    }
    Ok(())
}

/// Prepara uma cópia sem o [`Store`]: hash, normalização, miniatura e, para
/// imagem, a gravação de `<hash>.<ext>` e `<hash>.thumb.png` em
/// `files_dir/images/` (pulando o que já existe). Uma imagem cuja miniatura
/// falha (tipo sem decodificador, bomba de pixels, arquivo quebrado) entra sem
/// miniatura e sem dimensões.
pub fn prepare(clip: NewClip, files_dir: &Path) -> Result<Prepared, String> {
    let empty = |kind, hash, size| Prepared {
        kind,
        hash,
        text: None,
        norm: None,
        mime: None,
        width: None,
        height: None,
        size,
        lines: None,
        file: None,
        image: None,
    };
    match clip {
        NewClip::Text(text) => {
            let base = empty(Kind::Text, hash_of(Kind::Text, text.as_bytes()), 0);
            Ok(Prepared {
                norm: Some(normalize(&text)),
                size: text.chars().count(),
                lines: Some(text.lines().count().max(1)),
                text: Some(text),
                ..base
            })
        }
        NewClip::Files(list) => {
            let uris: Vec<String> = uri_lines(&list).into_iter().map(String::from).collect();
            let base = empty(
                Kind::Files,
                hash_of(Kind::Files, list.as_bytes()),
                list.len(),
            );
            Ok(Prepared {
                norm: Some(normalize(&file_names(&uris))),
                text: Some(list),
                ..base
            })
        }
        NewClip::Image { mime, bytes } => {
            let hash = hash_of(Kind::Image, &bytes);
            let file = format!("{hash}.{}", extension_for(&mime.to_ascii_lowercase()));
            let thumb = thumbnail(&bytes, &mime).ok();
            let prepared = Prepared {
                width: thumb.as_ref().map(|t| t.width),
                height: thumb.as_ref().map(|t| t.height),
                mime: Some(mime),
                file: Some(file),
                ..empty(Kind::Image, hash, bytes.len())
            };
            let prepared = Prepared {
                image: Some((bytes, thumb.map(|t| t.png))),
                ..prepared
            };
            write_image_files(&images_in(files_dir), &prepared)?;
            Ok(prepared)
        }
    }
}

/// Apaga o histórico do disco em `dir`: `history.db`, `-wal`, `-shm` e
/// `images/`.
pub fn wipe(dir: &Path) -> Result<(), String> {
    for name in [
        DB_FILE.to_string(),
        format!("{DB_FILE}-wal"),
        format!("{DB_FILE}-shm"),
        format!("{DB_FILE}.tmp"),
    ] {
        let path = dir.join(name);
        remove_quietly(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let images = images_in(dir);
    remove_dir_quietly(&images).map_err(|e| format!("{}: {e}", images.display()))
}

/// O conteúdo de um item para servir a seleção: o tipo, o mime (imagem) e os
/// bytes.
pub type Content = (Kind, Option<String>, Vec<u8>);

/// Uma página do `clipboard.list`.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub kind: Option<Kind>,
    pub query: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

/// Os itens já no JSON do protocolo e o total que casa com a consulta.
#[derive(Debug, Clone)]
pub struct Page {
    pub items: Vec<Value>,
    pub total: usize,
}

/// Uma linha da tabela `items`.
struct Record {
    id: i64,
    kind: Kind,
    hash: String,
    text: Option<String>,
    mime: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    size: i64,
    lines: Option<i64>,
    file: Option<String>,
    created: i64,
    used: i64,
    pinned: bool,
}

impl Record {
    fn from_row(row: &Row) -> rusqlite::Result<Record> {
        let kind: String = row.get(1)?;
        Ok(Record {
            id: row.get(0)?,
            kind: Kind::parse(&kind).unwrap_or(Kind::Text),
            hash: row.get(2)?,
            text: row.get(3)?,
            mime: row.get(4)?,
            width: row.get(5)?,
            height: row.get(6)?,
            size: row.get(7)?,
            lines: row.get(8)?,
            file: row.get(9)?,
            created: row.get(10)?,
            used: row.get(11)?,
            pinned: row.get(12)?,
        })
    }

    /// Os arquivos do item, dentro de `images/`.
    fn files(&self) -> Vec<String> {
        match &self.file {
            Some(file) => vec![file.clone(), thumb_name(&self.hash)],
            None => Vec::new(),
        }
    }

    /// O item no JSON do protocolo; com `query`, a prévia vem com os trechos
    /// marcados; com `full`, o texto inteiro.
    fn to_json(&self, images: &Path, query: Option<&str>, full: bool) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        out.insert("kind".into(), json!(self.kind.as_str()));
        let text = self.text.as_deref().unwrap_or("");
        let shown = match self.kind {
            Kind::Text => Some(text.to_string()),
            Kind::Files => {
                let uris = uri_lines(text);
                let names: Vec<String> = uris
                    .iter()
                    .map(|uri| file_names(&[uri.to_string()]))
                    .collect();
                out.insert("uris".into(), json!(uris));
                Some(names.join(", "))
            }
            Kind::Image => None,
        };
        if let Some(shown) = shown {
            let (preview, matches) = preview_for(&shown, query);
            out.insert("preview".into(), json!(preview));
            if query.is_some() {
                out.insert("matches".into(), json!(matches));
            }
        }
        if full && self.kind == Kind::Text {
            out.insert("text".into(), json!(text));
        }
        if let Some(file) = &self.file {
            out.insert("image".into(), json!(images.join(file)));
            if self.width.is_some() {
                out.insert("thumb".into(), json!(images.join(thumb_name(&self.hash))));
            }
        }
        let optional = [
            ("mime", self.mime.as_ref().map(|m| json!(m))),
            ("width", self.width.map(|w| json!(w))),
            ("height", self.height.map(|h| json!(h))),
        ];
        for (key, value) in optional {
            if let Some(value) = value {
                out.insert(key.into(), value);
            }
        }
        out.insert("size".into(), json!(self.size));
        if let Some(lines) = self.lines {
            out.insert("lines".into(), json!(lines));
        }
        out.insert("created".into(), json!(self.created));
        out.insert("used".into(), json!(self.used));
        out.insert("pinned".into(), json!(self.pinned));
        Value::Object(out)
    }
}

/// O histórico aberto.
pub struct Store {
    conn: Connection,
    origin: Origin,
    limit: usize,
}

impl Store {
    /// Abre o histórico. No disco: `history.db` e `images/` (criados com modo
    /// `0700`), com os órfãos de `images/` apagados. Na memória: apaga antes o
    /// histórico do disco e o que houver em `files/images/`.
    pub fn open(origin: Origin, limit: usize) -> Result<Store, String> {
        let conn = match &origin {
            Origin::Disk(dir) => {
                private_dir(dir)?;
                private_dir(&images_in(dir))?;
                let conn = Connection::open(dir.join(DB_FILE)).map_err(err)?;
                conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
                    .map_err(err)?;
                conn.pragma_update(None, "synchronous", "NORMAL")
                    .map_err(err)?;
                conn
            }
            Origin::Memory { files, disk } => {
                wipe(disk)?;
                private_dir(files)?;
                let images = images_in(files);
                remove_dir_quietly(&images).map_err(|e| format!("{}: {e}", images.display()))?;
                private_dir(&images)?;
                Connection::open_in_memory().map_err(err)?
            }
        };
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(err)?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "o histórico é da versão {version}, mais nova que a {SCHEMA_VERSION}"
            ));
        }
        conn.execute_batch(SCHEMA).map_err(err)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(err)?;
        let mut store = Store {
            conn,
            origin,
            limit,
        };
        store.remove_orphans()?;
        store.set_limit(limit)?;
        Ok(store)
    }

    /// A pasta onde o [`prepare`] grava (a pasta pai de `images/`).
    pub fn files_dir(&self) -> PathBuf {
        match &self.origin {
            Origin::Disk(dir) => dir.clone(),
            Origin::Memory { files, .. } => files.clone(),
        }
    }

    fn images(&self) -> PathBuf {
        images_in(&self.files_dir())
    }

    fn remove_orphans(&self) -> Result<(), String> {
        let mut known = std::collections::HashSet::new();
        let mut stmt = self
            .conn
            .prepare("SELECT hash, file FROM items WHERE file IS NOT NULL")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(err)?;
        for row in rows {
            let (hash, file) = row.map_err(err)?;
            known.insert(file);
            known.insert(thumb_name(&hash));
        }
        let images = self.images();
        for entry in fs::read_dir(&images).map_err(|e| format!("{}: {e}", images.display()))? {
            let entry = entry.map_err(err)?;
            if !known.contains(&*entry.file_name().to_string_lossy()) {
                let _ = remove_quietly(&entry.path());
            }
        }
        Ok(())
    }

    fn delete_files(&self, files: &[String]) {
        let images = self.images();
        for file in files {
            let _ = remove_quietly(&images.join(file));
        }
    }

    /// Os não fixados acima do limite saem, por `used` mais antigo; devolve os
    /// arquivos deles para apagar depois do `COMMIT`.
    fn evict(tx: &Transaction, limit: usize) -> Result<Vec<String>, String> {
        let mut stmt = tx
            .prepare(&format!(
                "SELECT {COLUMNS} FROM items WHERE pinned = 0
                 ORDER BY used DESC, id DESC LIMIT -1 OFFSET ?1"
            ))
            .map_err(err)?;
        let old: Vec<Record> = stmt
            .query_map([limit as i64], Record::from_row)
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        let mut files = Vec::new();
        for record in old {
            tx.execute("DELETE FROM items WHERE id = ?1", [record.id])
                .map_err(err)?;
            files.extend(record.files());
        }
        Ok(files)
    }

    /// Grava a cópia preparada: o id e se é nova. A repetida (mesmo hash) só
    /// ganha `used = now_ms`, com o mesmo id e o mesmo `pinned`. Aplica o
    /// limite na mesma transação.
    pub fn insert(&mut self, p: Prepared, now_ms: i64) -> Result<(i64, bool), String> {
        let tx = self.conn.transaction().map_err(err)?;
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM items WHERE hash = ?1", [&p.hash], |row| {
                row.get(0)
            })
            .optional()
            .map_err(err)?;
        let (id, new) = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE items SET used = ?1 WHERE id = ?2",
                    params![now_ms, id],
                )
                .map_err(err)?;
                (id, false)
            }
            None => {
                tx.execute(
                    "INSERT INTO items
                     (kind, hash, text, norm, mime, width, height, size, lines, file, created, used)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        p.kind.as_str(),
                        p.hash,
                        p.text,
                        p.norm,
                        p.mime,
                        p.width,
                        p.height,
                        p.size as i64,
                        p.lines.map(|l| l as i64),
                        p.file,
                        now_ms,
                    ],
                )
                .map_err(err)?;
                (tx.last_insert_rowid(), true)
            }
        };
        let evicted = Self::evict(&tx, self.limit)?;
        tx.commit().map_err(err)?;
        self.delete_files(&evicted);
        // Normalmente o `prepare` já gravou; só falta algo se a pasta mudou no
        // meio (a persistência trocada) ou se o mesmo item saiu pelo limite
        // enquanto esta cópia era preparada.
        write_image_files(&self.images(), &p)?;
        Ok((id, new))
    }

    /// Sobe o item ao topo; `false` se ele não existe.
    pub fn touch(&mut self, id: i64, now_ms: i64) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE items SET used = ?1 WHERE id = ?2",
                params![now_ms, id],
            )
            .map_err(err)?;
        Ok(changed > 0)
    }

    /// Apaga o item e os arquivos dele; `false` se ele não existe.
    pub fn remove(&mut self, id: i64) -> Result<bool, String> {
        let Some(record) = self.record(id)? else {
            return Ok(false);
        };
        self.conn
            .execute("DELETE FROM items WHERE id = ?1", [id])
            .map_err(err)?;
        self.delete_files(&record.files());
        Ok(true)
    }

    /// Fixa ou solta o item; `false` se ele não existe. Soltar pode tirar
    /// itens pelo limite.
    pub fn pin(&mut self, id: i64, pinned: bool) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE items SET pinned = ?1 WHERE id = ?2",
                params![pinned, id],
            )
            .map_err(err)?;
        if changed > 0 && !pinned {
            self.set_limit(self.limit)?;
        }
        Ok(changed > 0)
    }

    /// Apaga os não fixados (ou todos, com `all`) e os arquivos deles; devolve
    /// quantos saíram.
    pub fn clear(&mut self, all: bool) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(err)?;
        let filter = if all { "" } else { " WHERE pinned = 0" };
        let records: Vec<Record> = {
            let mut stmt = tx
                .prepare(&format!("SELECT {COLUMNS} FROM items{filter}"))
                .map_err(err)?;
            stmt.query_map([], Record::from_row)
                .map_err(err)?
                .collect::<Result<_, _>>()
                .map_err(err)?
        };
        tx.execute(&format!("DELETE FROM items{filter}"), [])
            .map_err(err)?;
        tx.commit().map_err(err)?;
        let files: Vec<String> = records.iter().flat_map(Record::files).collect();
        self.delete_files(&files);
        Ok(records.len())
    }

    /// Troca o limite e já o aplica.
    pub fn set_limit(&mut self, limit: usize) -> Result<(), String> {
        self.limit = limit;
        let tx = self.conn.transaction().map_err(err)?;
        let evicted = Self::evict(&tx, limit)?;
        tx.commit().map_err(err)?;
        self.delete_files(&evicted);
        Ok(())
    }

    /// O total de itens e quantos estão fixados.
    pub fn counts(&self) -> Result<(usize, usize), String> {
        self.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(pinned), 0) FROM items",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as usize,
                        row.get::<_, i64>(1)? as usize,
                    ))
                },
            )
            .map_err(err)
    }

    /// Uma página: fixados primeiro, depois por uso mais recente. Com
    /// consulta, imagens nunca casam; de 3 caracteres (normalizados) para
    /// cima, pelo índice trigram, com a consulta entre aspas (sem a sintaxe
    /// do FTS5); com 1 ou 2, pela comparação direta no texto normalizado.
    pub fn list(&self, q: &ListQuery) -> Result<Page, String> {
        let query = q.query.as_deref().filter(|raw| !normalize(raw).is_empty());
        let mut conds: Vec<&str> = Vec::new();
        let mut args: Vec<Sql> = Vec::new();
        if let Some(kind) = q.kind {
            conds.push("kind = ?");
            args.push(Sql::Text(kind.as_str().into()));
        }
        if let Some(raw) = query {
            let norm = normalize(raw);
            conds.push("kind != 'image'");
            if norm.chars().count() > SHORT_QUERY {
                conds.push("id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)");
                args.push(Sql::Text(format!("\"{}\"", norm.replace('"', "\"\""))));
            } else {
                conds.push("instr(norm, ?) > 0");
                args.push(Sql::Text(norm));
            }
        }
        let filter = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        let total: i64 = self
            .conn
            .query_row(
                &format!("SELECT COUNT(*) FROM items{filter}"),
                params_from_iter(&args),
                |row| row.get(0),
            )
            .map_err(err)?;
        let mut page_args = args.clone();
        page_args.push(Sql::Integer(q.limit as i64));
        page_args.push(Sql::Integer(q.offset as i64));
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM items{filter}
                 ORDER BY pinned DESC, used DESC, id DESC LIMIT ? OFFSET ?"
            ))
            .map_err(err)?;
        let images = self.images();
        let items = stmt
            .query_map(params_from_iter(&page_args), Record::from_row)
            .map_err(err)?
            .map(|record| record.map(|r| r.to_json(&images, query, false)))
            .collect::<Result<_, _>>()
            .map_err(err)?;
        Ok(Page {
            items,
            total: total as usize,
        })
    }

    fn record(&self, id: i64) -> Result<Option<Record>, String> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM items WHERE id = ?1"),
                [id],
                Record::from_row,
            )
            .optional()
            .map_err(err)
    }

    /// O item com o texto inteiro (texto) ou as `uris` (arquivos).
    pub fn get(&self, id: i64) -> Result<Option<Value>, String> {
        Ok(self
            .record(id)?
            .map(|r| r.to_json(&self.images(), None, true)))
    }

    /// O que o `offers_for` precisa: o tipo, o mime (imagem) e os bytes (o
    /// texto, a `text/uri-list` crua ou o arquivo da imagem).
    pub fn content(&self, id: i64) -> Result<Option<Content>, String> {
        let Some(record) = self.record(id)? else {
            return Ok(None);
        };
        let bytes = match &record.file {
            Some(file) => {
                let path = self.images().join(file);
                fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?
            }
            None => record.text.clone().unwrap_or_default().into_bytes(),
        };
        Ok(Some((record.kind, record.mime, bytes)))
    }

    /// Troca onde o histórico mora. Memória → disco: apaga o destino, copia o
    /// banco (`VACUUM INTO`) e os arquivos (copiados, não renomeados: o tmpfs
    /// e o disco são sistemas de arquivos diferentes). Disco → memória: apaga
    /// o disco e começa vazio.
    pub fn move_to(self, origin: Origin) -> Result<Store, String> {
        if self.origin == origin {
            return Ok(self);
        }
        let limit = self.limit;
        match (&self.origin, &origin) {
            (Origin::Memory { files, .. }, Origin::Disk(dir)) => {
                wipe(dir)?;
                private_dir(dir)?;
                let target = images_in(dir);
                private_dir(&target)?;
                let tmp = dir.join(format!("{DB_FILE}.tmp"));
                self.conn
                    .execute("VACUUM INTO ?1", [tmp.to_string_lossy()])
                    .map_err(err)?;
                let source = images_in(files);
                for entry in
                    fs::read_dir(&source).map_err(|e| format!("{}: {e}", source.display()))?
                {
                    let entry = entry.map_err(err)?;
                    let to = target.join(entry.file_name());
                    let copy =
                        to.with_file_name(format!(".{}.copy", entry.file_name().to_string_lossy()));
                    fs::copy(entry.path(), &copy)
                        .and_then(|_| fs::rename(&copy, &to))
                        .map_err(|e| format!("{}: {e}", to.display()))?;
                }
                fs::rename(&tmp, dir.join(DB_FILE)).map_err(err)?;
                drop(self);
                Store::open(origin, limit)
            }
            (old, _) => {
                let old = old.clone();
                drop(self);
                if let Origin::Disk(dir) = old {
                    wipe(&dir)?;
                }
                Store::open(origin, limit)
            }
        }
    }
}

impl Drop for Store {
    /// Sem persistência, os arquivos somem junto (também quando o módulo é
    /// abortado sem passar por código de saída).
    fn drop(&mut self) {
        if let Origin::Memory { files, .. } = &self.origin {
            let _ = remove_dir_quietly(&images_in(files));
            let _ = fs::remove_dir(files);
        }
    }
}
