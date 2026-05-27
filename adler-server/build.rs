//! Mirror the workspace-sibling `../adler-web/dist/` SPA bundle into
//! this crate's own `dist/` so that `rust-embed` can pick it up at a
//! path that survives `cargo publish` (which strips files outside the
//! package root).
//!
//! Two scenarios:
//!
//!   * **Workspace build** — the sibling exists. We refresh `dist/`
//!     from it so a `cd adler-web && npm run build` followed by
//!     `cargo build -p adler-server` is enough.
//!
//!   * **crates.io / standalone build** — the sibling doesn't exist
//!     (the tarball only ships `adler-server/`). We leave whatever
//!     was packaged in `dist/` alone, which is the SPA bundle
//!     produced by the release pipeline just before `cargo publish`.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"),
    );
    let sibling = manifest.join("..").join("adler-web").join("dist");
    let local = manifest.join("dist");

    println!("cargo:rerun-if-changed=../adler-web/dist");
    println!("cargo:rerun-if-changed=build.rs");

    if !sibling.is_dir() {
        return;
    }

    if let Err(e) = mirror(&sibling, &local) {
        panic!(
            "adler-server build.rs: failed to mirror {} -> {}: {e}",
            sibling.display(),
            local.display()
        );
    }
}

fn mirror(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    copy_recursive(src, dst)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
