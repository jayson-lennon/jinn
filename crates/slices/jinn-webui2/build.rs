//! Build script: builds the web UI client bundle and embeds it, beside the
//! committed icon set the page shell and web manifest name.
//!
//! The bundle is build **output** of `webui/client2`. This script produces
//! it rather than reading a committed copy: it installs the client's
//! dependencies when the lock file is newer than them, type-checks,
//! runs the client's unit tests, runs `vite build`, gzips the output into
//! `OUT_DIR`, and generates the `include_bytes!` table the server embeds.
//! The icons under `resources/` are committed input, not build output, and
//! are embedded as their own bytes in the same table. `resources/icon.svg`
//! is their master: the figure is composited centred on a full-bleed
//! `#25272e` tile (the manifest's `theme_color`) and rasterised with
//! `rsvg-convert` at 4x, then Lanczos-downsampled to each committed size.
//! Nothing in-repo runs that — it is a hand step when the mark changes.
//!
//! The build runs on every compile, unconditionally — the client's type-check,
//! unit tests, and bundle take well under a second, and that is the price of the one
//! property this design buys: the bundle cannot be stale, because it is
//! always built from the sources being compiled. There is no committed
//! bundle to drift, no pin to refresh, and no merge conflict on build output.
//! A failing unit test fails the build by its own name, the same way a type
//! error does, so no bundle ships from logic a test says is broken.
//!
//! Node and npm are therefore build prerequisites of this crate. A missing
//! `npm` fails the build with a message naming the requirement rather than
//! surfacing as a bare exec error.
//!
//! A build script's panic is its failure channel: every failure must fail
//! the build loudly, not quietly produce a binary serving broken bytes.

#![allow(clippy::panic, reason = "build script: panic is the failure channel")]
#![allow(
    clippy::expect_used,
    reason = "build script: env and IO failures are fatal by contract"
)]
#![allow(
    clippy::disallowed_types,
    reason = "build-script env plumbing is the only way to reach CARGO_MANIFEST_DIR and OUT_DIR"
)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::Digest as _;

/// The client's directory relative to the repository root.
const CLIENT_DIR: &str = "webui/client2";

/// How an asset's embedded bytes relate to the file they come from.
#[derive(Clone, Copy)]
enum Encoding {
    /// The bytes are the gzip of the built file; the server sends
    /// `Content-Encoding: gzip`.
    Gzip,
    /// The bytes are the committed file's own; the server sends no
    /// `Content-Encoding`. PNG and JSON want no compression at this size,
    /// and a spurious gzip header would make the browser try to inflate
    /// raw bytes.
    Identity,
}

/// One served asset: its name under `/vendor/`, its MIME type, and how
/// its bytes are embedded.
struct Asset {
    served: &'static str,
    content_type: &'static str,
    encoding: Encoding,
}

/// The two built client files, then the committed icon set the page shell
/// and the web manifest name. Every one is linked by `name.<sha256>.ext`,
/// so a changed file is a new URL and a cached copy can never outlive it.
///
/// The service worker is the one exception to that rule: it is served at a
/// fixed, unhashed URL (`/sw.js`) and revalidated on every fetch, because a
/// browser only checks for a new worker at the URL it registered. It still
/// rides this table so its bytes are embedded and pinned like the rest.
const ASSETS: [Asset; 8] = [
    Asset {
        served: "client2.js",
        content_type: "text/javascript; charset=utf-8",
        encoding: Encoding::Gzip,
    },
    Asset {
        served: "client2.css",
        content_type: "text/css; charset=utf-8",
        encoding: Encoding::Gzip,
    },
    Asset {
        served: "sw.js",
        content_type: "text/javascript; charset=utf-8",
        encoding: Encoding::Gzip,
    },
    Asset {
        served: "icon-192.png",
        content_type: "image/png",
        encoding: Encoding::Identity,
    },
    Asset {
        served: "icon-512.png",
        content_type: "image/png",
        encoding: Encoding::Identity,
    },
    Asset {
        served: "icon-maskable-512.png",
        content_type: "image/png",
        encoding: Encoding::Identity,
    },
    Asset {
        served: "icon-apple-touch-180.png",
        content_type: "image/png",
        encoding: Encoding::Identity,
    },
    Asset {
        served: "icon-favicon-32.png",
        content_type: "image/png",
        encoding: Encoding::Identity,
    },
];

/// The web manifest, as committed under `templates/`. The server renders
/// and embeds it itself — it names each icon by a hash only this build
/// knows — so it is not an asset row; it is listed for re-runs only.
const MANIFEST_TEMPLATE: &str = "templates/manifest.webmanifest";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    // `<repo>/webui/client2`: this crate sits at crates/slices/<name>, so the
    // repository root is three levels up.
    let client_dir = manifest_dir.join("../../../").join(CLIENT_DIR);
    assert!(client_dir.is_dir(), "{CLIENT_DIR} is missing");

    // Re-run when anything that could change the bundle changes. The lock
    // file is listed explicitly: Cargo never sees `node_modules`, so a
    // dependency bump would otherwise leave a cached build in place.
    for name in source_files(&client_dir) {
        println!("cargo:rerun-if-changed={}", client_dir.join(name).display());
    }
    // And when a committed asset changes. An icon's `include_bytes!` sits
    // inside a generated file in `OUT_DIR`, pulled in through `include!`,
    // and whether rustc records that nested path in dep-info is not
    // something to rely on; the manifest template is read by the server
    // the same way. One line each, and the rebuild is correct by construction.
    for asset in &ASSETS {
        if matches!(asset.encoding, Encoding::Identity) {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir.join("resources").join(asset.served).display()
            );
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(MANIFEST_TEMPLATE).display()
    );

    install_dependencies(&client_dir);
    build_client(&client_dir);

    // `include_bytes!` must name a path the compiler can resolve at compile
    // time. `OUT_DIR` is an absolute path cargo supplies, so the generated
    // file reaches it through `env!`/`concat!` — the same way the server
    // reaches this file — rather than through a rendered absolute path,
    // which would carry backslashes on Windows.
    let bundle_dir = out_dir.join("client2");
    let mut table = String::from(
        "// @generated by build.rs from the web UI client build — do not edit.\n\
         // One row per served asset; `include_bytes!` is emitted relative to OUT_DIR\n\
         // or the crate directory.\n\n\
         /// The complete client inventory: `path` is the `/vendor/`-relative served\n\
         /// path, `sha256` the pin the hashed URL carries, `bytes` the embedded\n\
         /// bytes, `content_type` its MIME type, and `gzip` whether `bytes` are\n\
         /// gzip-compressed (the server sends `Content-Encoding: gzip` only so).\n\
         #[derive(Clone, Copy)]\n\
         pub struct VendorAsset {\n    \
             pub path: &'static str,\n    \
             pub sha256: &'static str,\n    \
             pub bytes: &'static [u8],\n    \
             pub content_type: &'static str,\n    \
             pub gzip: bool,\n\
         }\n\n\
         pub const VENDOR_ASSETS: &[VendorAsset] = &[\n",
    );
    for asset in &ASSETS {
        // The hash is of the exact bytes embedded below, so the URL the
        // shell names and the bytes it is served can never disagree.
        let (sha256, bytes_expr, gzip) = match asset.encoding {
            Encoding::Gzip => (
                gzip_asset(
                    &client_dir.join("dist").join(asset.served),
                    &bundle_dir.join(format!("{}.gz", asset.served)),
                ),
                format!(
                    "include_bytes!(concat!(env!(\"OUT_DIR\"), \"/client2/{}.gz\"))",
                    asset.served
                ),
                true,
            ),
            Encoding::Identity => (
                identity_sha256(&manifest_dir.join("resources").join(asset.served)),
                format!(
                    "include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/resources/{}\"))",
                    asset.served
                ),
                false,
            ),
        };
        let _ = writeln!(
            table,
            "    VendorAsset {{\n        path: \"{served}\",\n        sha256: \"{sha256}\",\n        \
             bytes: {bytes_expr},\n        content_type: \"{content_type}\",\n        gzip: {gzip},\n    }},",
            served = asset.served,
            content_type = asset.content_type,
        );
    }
    let _ = writeln!(table, "];");

    fs::write(out_dir.join("vendor_assets.rs"), table)
        .expect("writing vendor_assets.rs into OUT_DIR cannot fail");
}

/// `npm ci` when the installed dependencies are absent or older than the
/// lock file. The same rule the e2e recipes use, so one lock-file edit
/// re-installs everywhere and never anywhere twice in a row.
///
/// # Panics
///
/// Panics when `npm` is not on `PATH`, or the install exits non-zero: the
/// client cannot be built without its dependencies, and a half-installed
/// tree must not be built against.
fn install_dependencies(client_dir: &Path) {
    let node_modules = client_dir.join("node_modules");
    let lock = client_dir.join("package-lock.json");
    let stale = !node_modules.is_dir() || lock_is_newer(&lock, &node_modules);
    if !stale {
        return;
    }
    println!("cargo:warning=web UI client: installing dependencies (npm ci)");
    run(Command::new("npm")
        .args(["ci", "--no-fund", "--no-audit"])
        .current_dir(client_dir));
}

/// Whether `lock` was modified after `dir` was — the install is stale when so.
fn lock_is_newer(lock: &Path, dir: &Path) -> bool {
    let (Ok(lock_time), Ok(dir_time)) = (fs::metadata(lock), fs::metadata(dir)) else {
        // Unreadable metadata is treated as stale: an install that was not
        // needed costs a second, a build against an unknown tree costs a
        // debugging session.
        return true;
    };
    let (Ok(lock_time), Ok(dir_time)) = (lock_time.modified(), dir_time.modified()) else {
        return true;
    };
    lock_time > dir_time
}

/// Type-checks, tests, and bundles the client. `tsc` runs first so a type
/// error fails the cargo build by its own name instead of surfacing later as
/// a misbehaving page; the unit tests run next, for the same reason.
///
/// # Panics
///
/// Panics when `tsc`, `vitest`, or `vite` exits non-zero, naming the tool
/// and the captured output.
fn build_client(client_dir: &Path) {
    let bin = client_dir.join("node_modules").join(".bin");
    run(Command::new(bin.join("tsc"))
        .args(["--noEmit", "-p", "tsconfig.json"])
        .current_dir(client_dir));
    // The client's unit tests. They run on every compile, after the type
    // check and before the bundle: a red test fails the build by its own
    // name, the same way a type error does, and no bundle is built from
    // logic a test says is broken. `vitest.config.mjs` is the config; the
    // production `vite.config.mjs` names no tests.
    run(Command::new(bin.join("vitest"))
        .arg("run")
        .current_dir(client_dir));
    run(Command::new(bin.join("vite"))
        .arg("build")
        .current_dir(client_dir));
}

/// Runs a command and panics with its captured output when it fails.
///
/// # Panics
///
/// Panics when the program cannot be spawned — naming the likely cause, a
/// missing Node toolchain — or when it exits non-zero, with stdout and
/// stderr so the build log carries the real error.
fn run(command: &mut Command) {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            panic!(
                "`{program}` was not found: building the web UI client needs Node and npm on PATH \
                 (this crate's build.rs runs the client build itself)"
            )
        }
        Err(error) => panic!("running `{program}`: {error}"),
    };
    if !output.status.success() {
        let mut message = format!("`{program}` failed ({})", output.status);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            let _ = write!(message, "\n--- stdout ---\n{}", stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            let _ = write!(message, "\n--- stderr ---\n{}", stderr.trim_end());
        }
        panic!("{message}");
    }
}

/// Gzips one built asset into the bundle directory, verifying the bytes on
/// the way: the build output must exist, be non-empty, and the compressed
/// result must carry the gzip magic the server's `Content-Encoding: gzip`
/// promises. Returns the compressed bytes' sha256 as hex — the pin the
/// page shell names the asset by.
///
/// # Panics
///
/// Panics when the built asset is missing or empty, or when the gzip
/// encoder fails — the bundle is the thing this script exists to produce,
/// so any of those is a broken build, never a degraded one.
fn gzip_asset(built: &Path, gzipped: &Path) -> String {
    let bytes = fs::read(built).unwrap_or_else(|error| {
        panic!(
            "the web UI client build did not produce {} ({error}) — check the vite output above",
            built.display()
        )
    });
    assert!(!bytes.is_empty(), "{} is empty", built.display());
    if let Some(parent) = gzipped.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|error| panic!("creating {}: {error}", parent.display()));
    }
    let file = fs::File::create(gzipped)
        .unwrap_or_else(|error| panic!("creating {}: {error}", gzipped.display()));
    let mut encoder = GzEncoder::new(file, Compression::new(9));
    encoder
        .write_all(&bytes)
        .unwrap_or_else(|error| panic!("gzipping {}: {error}", built.display()));
    encoder
        .finish()
        .unwrap_or_else(|error| panic!("finishing {}: {error}", gzipped.display()));
    let compressed = fs::read(gzipped)
        .unwrap_or_else(|error| panic!("re-reading {}: {error}", gzipped.display()));
    assert!(
        compressed.starts_with(&[0x1f, 0x8b]),
        "{} is not gzip (missing the 1f 8b magic prefix)",
        gzipped.display()
    );
    let mut hex = String::with_capacity(64);
    for byte in sha2::Sha256::digest(&compressed) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The sha256 of a committed asset's own bytes, as lowercase hex — the pin
/// the hashed URL carries, and the hash of exactly what the server embeds.
/// No copy is made: the generated table's `include_bytes!` names the file
/// in place.
///
/// # Panics
///
/// Panics when the committed file is missing or empty — a missing icon is a
/// broken install set, never a degraded one.
fn identity_sha256(file: &Path) -> String {
    let bytes = fs::read(file).unwrap_or_else(|error| {
        panic!(
            "a committed web UI asset is missing: {} ({error})",
            file.display()
        )
    });
    assert!(!bytes.is_empty(), "{} is empty", file.display());
    let mut hex = String::with_capacity(64);
    for byte in sha2::Sha256::digest(&bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Every regular file under `root`, as `base`-relative forward-slash paths.
fn collect_files(root: &Path, base: &Path, out: &mut Vec<String>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|error| panic!("reading {}: {error}", root.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("reading {}: {error}", root.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, base, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .expect("collected under base")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push(rel);
        }
    }
}

/// The files whose content defines the client, as client-dir-relative
/// forward-slash paths, sorted: exactly the bundle's inputs — the sources,
/// the build config, and the lock file.
///
/// `e2e/`, `dist/` and `node_modules/` are deliberately excluded: a spec
/// never changes one byte of the bundle, the dist is the output itself, and
/// the installed dependencies are already covered by the lock file.
fn source_files(client_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let src = client_dir.join("src");
    if src.is_dir() {
        collect_files(&src, client_dir, &mut names);
    }
    for name in [
        "index.html",
        "package.json",
        "package-lock.json",
        "vite.config.mjs",
        "vitest.config.mjs",
        "tsconfig.json",
    ] {
        if client_dir.join(name).exists() {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}
