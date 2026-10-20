//! Ajudantes para os testes (feature `testing`).

use std::path::PathBuf;

/// O caminho de `rel` dentro do `testdata/` deste crate.
pub fn testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(rel)
}
