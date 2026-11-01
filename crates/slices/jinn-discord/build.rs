//! Build script: creates a SQLite DB with the post-migration schema and
//! exposes its path as `DAOW_DATABASE_URL` so the `#[dao]` macro can
//! validate the thread-map's SQL at compile time. Same schema-crate
//! pattern as the kernel's build script.

#![allow(warnings, reason = "want to fail fast")]

use std::path::Path;

fn main() {
    #[expect(
        clippy::disallowed_types,
        reason = "OUT_DIR is cargo's own env plumbing, consumed only to place the dao validation database"
    )]
    let dir =
        std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("dao_validation");
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

    // Schema sources: declared relative to this crate's manifest, never to
    // the cwd. A rerun-if-changed path that does not exist makes cargo
    // treat the build script as always-dirty (recompile on every build),
    // silently and permanently — so the build aborts instead.
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
