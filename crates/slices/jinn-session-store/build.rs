//! Build script: creates a SQLite DB with the post-v20 schema and exposes its
//! path as `DAOW_DATABASE_URL` so the `#[dao]` macro can validate `#[query]` /
//! `#[execute]` SQL against a real database at compile time.
//!
//! The schema is sourced from `jinn_session_schema::run_migrations` — the
//! single source of truth shared with the runtime migrator. No hand-written
//! `.sql` file to drift out of sync. See the `dao` crate's README ("The
//! schema-crate pattern") for the rationale.

#![allow(warnings, reason = "want to fail fast")]

use std::path::Path;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("dao_validation");
    std::fs::create_dir_all(&dir).expect("create dao_validation dir");

    let db_path = dir.join("validation.db");
    // Always recreate so the schema is current.
    let _ = std::fs::remove_file(&db_path);

    let mut conn = rusqlite::Connection::open(&db_path)
        .unwrap_or_else(|e| panic!("failed to open dao validation db: {e}"));
    jinn_session_schema::run_migrations(&mut conn)
        .unwrap_or_else(|e| panic!("failed to apply migrations to dao validation db: {e}"));

    println!(
        "cargo:rustc-env=DAOW_DATABASE_URL={}",
        db_path.to_string_lossy()
    );

    // Rerun whenever the schema crate's source changes so new/edited migrations
    // appear in the validation DB on the next build. Declared relative to this
    // crate's manifest, never to the cwd. A rerun-if-changed path that does
    // not exist makes cargo treat the build script as always-dirty (recompile
    // on every build), silently and permanently — so the build aborts instead.
    for name in ["lib.rs", "migrate.rs", "legacy.rs"] {
        let path = Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .join("../../jinn-session-schema/src")
            .join(name);
        assert!(
            path.exists(),
            "build script rerun path does not exist: {} (resolved from CARGO_MANIFEST_DIR)",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
