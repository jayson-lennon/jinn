//! Dev-time vendor fetcher for the canvas slice.
//!
//! Run through `just canvas-vendor-update`, never by hand: the recipe drives
//! this binary, which downloads every pinned file listed in
//! [`VENDOR_SOURCES`], gzip-wraps the library dists deterministically (fixed
//! mtime, no filename, level 9 — byte-identical output for identical input,
//! so the committed bytes change only when upstream does), and rewrites
//! `resources/vendor/vendor-manifest.toml`. Licenses and the KaTeX woff2
//! fonts are vendored plain: fonts must keep their own gzip identity because
//! the server marks only the library dists `Content-Encoding: gzip`, and
//! licenses serve human-readable. The crate embeds exactly the bytes the
//! manifest hashes, so a manifest row and its file move together and every
//! refresh is a clean, reviewable diff.
//!
//! The manifest is the pin. Upgrading a library means bumping the version
//! in [`VENDOR_SOURCES`], rerunning the recipe, and committing the whole
//! result — files are never edited by hand.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::Digest;

/// One vendored file: its served path under `/vendor/`, the pinned npm
/// package version, and the package-relative dist path jsDelivr serves.
struct VendorSource {
    /// Path the canvas server serves the file at, `vendor/`-relative.
    served: String,
    /// npm package name (scoped names included verbatim).
    package: &'static str,
    /// Pinned version. Upgrades happen only through this table.
    version: &'static str,
    /// Package-relative file path (without leading slash).
    file: String,
}

/// A library row: the static half of a [`VendorSource`] (package pin and
/// package-relative dist path); the served name is derived from it.
const LIBRARIES: &[(&str, &str, &str)] = &[
    // (package, version, package-relative file) — served under <lib>/<file name>.
    ("chart.js", "4.6.1", "dist/chart.umd.js"),
    ("uplot", "1.6.32", "dist/uPlot.iife.min.js"),
    ("uplot", "1.6.32", "dist/uPlot.min.css"),
    ("vega", "6.4.0", "build/vega.min.js"),
    ("vega-lite", "6.5.0", "build/vega-lite.min.js"),
    ("vega-embed", "7.3.0", "build/vega-embed.min.js"),
    ("mermaid", "12.2.0", "dist/mermaid.min.js"),
    ("d3", "7.9.0", "dist/d3.min.js"),
    ("@observablehq/plot", "0.6.17", "dist/plot.umd.min.js"),
    ("katex", "0.19.1", "dist/katex.min.js"),
    ("katex", "0.19.1", "dist/katex.min.css"),
    ("p5", "1.11.13", "lib/p5.min.js"),
    ("vue", "3.5.44", "dist/vue.global.prod.js"),
    ("htm", "3.1.1", "preact/standalone.umd.js"),
    ("@highlightjs/cdn-assets", "11.12.0", "highlight.min.js"),
    (
        "@highlightjs/cdn-assets",
        "11.12.0",
        "styles/github.min.css",
    ),
    ("matter-js", "0.20.0", "build/matter.min.js"),
    // three.js core: the last line to ship a minified ES-module build
    // (r186 dropped the min dists), pinned to stay on it. Both halves of
    // the split build ride one pin: the main module statically imports
    // `./three.core.min.js` beside it, so vendoring only the main module
    // breaks every import graph. The bare `three` import map entry points
    // at the main module; addons keep importing the same bare specifier.
    ("three", "0.185.0", "build/three.core.min.js"),
    ("three", "0.185.0", "build/three.module.min.js"),
    // three.js addons under `examples/jsm/…` upstream; `served_for` maps
    // that prefix to `three/addons/…`, preserving the subtree shape —
    // the addons import each other by relative path (GLTFLoader →
    // ../utils/BufferGeometryUtils.js, ../utils/SkeletonUtils.js), so
    // the directory layout is load-bearing.
    ("three", "0.185.0", "examples/jsm/controls/OrbitControls.js"),
    (
        "three",
        "0.185.0",
        "examples/jsm/controls/PointerLockControls.js",
    ),
    ("three", "0.185.0", "examples/jsm/loaders/GLTFLoader.js"),
    ("three", "0.185.0", "examples/jsm/loaders/OBJLoader.js"),
    ("three", "0.185.0", "examples/jsm/loaders/STLLoader.js"),
    (
        "three",
        "0.185.0",
        "examples/jsm/geometries/ParametricGeometry.js",
    ),
    (
        "three",
        "0.185.0",
        "examples/jsm/geometries/ParametricFunctions.js",
    ),
    (
        "three",
        "0.185.0",
        "examples/jsm/utils/BufferGeometryUtils.js",
    ),
    ("three", "0.185.0", "examples/jsm/utils/SkeletonUtils.js"),
];

/// Library directory names under `/vendor/`, one per upstream package the
/// set vendors (the three vega crates share one directory; htm's preact
/// standalone serves as the preact+htm pairing).
const LIBRARY_DIRS: &[(&str, &str)] = &[
    ("chart.js", "chartjs"),
    ("uplot", "uplot"),
    ("vega", "vega"),
    ("vega-lite", "vega"),
    ("vega-embed", "vega"),
    ("mermaid", "mermaid"),
    ("d3", "d3"),
    ("@observablehq/plot", "plot"),
    ("katex", "katex"),
    ("p5", "p5"),
    ("vue", "vue"),
    ("htm", "preact"),
    ("@highlightjs/cdn-assets", "highlightjs"),
    ("matter-js", "matter"),
    ("three", "three"),
];

/// License rows: (package, version, package-relative file, served name).
/// Vendored plain beside the libs they cover (MIT/BSD/ISC terms require the
/// notice to travel with the code); hashed by the manifest like everything
/// else.
const LICENSES: &[(&str, &str, &str, &str)] = &[
    ("chart.js", "4.6.1", "LICENSE.md", "chartjs/LICENSE"),
    ("uplot", "1.6.32", "LICENSE", "uplot/LICENSE"),
    ("vega", "6.4.0", "LICENSE", "vega/vega.LICENSE"),
    ("vega-lite", "6.5.0", "LICENSE", "vega/vega-lite.LICENSE"),
    ("vega-embed", "7.3.0", "LICENSE", "vega/vega-embed.LICENSE"),
    ("mermaid", "12.2.0", "LICENSE", "mermaid/LICENSE"),
    ("d3", "7.9.0", "LICENSE", "d3/LICENSE"),
    ("@observablehq/plot", "0.6.17", "LICENSE", "plot/LICENSE"),
    ("katex", "0.19.1", "LICENSE", "katex/LICENSE"),
    ("p5", "1.11.13", "license.txt", "p5/license.txt"),
    ("vue", "3.5.44", "LICENSE", "vue/LICENSE"),
    ("htm", "3.1.1", "LICENSE", "preact/LICENSE"),
    (
        "@highlightjs/cdn-assets",
        "11.12.0",
        "LICENSE",
        "highlightjs/LICENSE",
    ),
    ("matter-js", "0.20.0", "LICENSE", "matter/LICENSE"),
    ("three", "0.185.0", "LICENSE", "three/LICENSE"),
];

/// The 20 woff2 fonts `katex.min.css` references by relative URL; the
/// directory layout (`katex/fonts/…`) is load-bearing for math rendering.
const KATEX_FONTS: &[&str] = &[
    "KaTeX_AMS-Regular",
    "KaTeX_Caligraphic-Bold",
    "KaTeX_Caligraphic-Regular",
    "KaTeX_Fraktur-Bold",
    "KaTeX_Fraktur-Regular",
    "KaTeX_Main-Bold",
    "KaTeX_Main-BoldItalic",
    "KaTeX_Main-Italic",
    "KaTeX_Main-Regular",
    "KaTeX_Math-BoldItalic",
    "KaTeX_Math-Italic",
    "KaTeX_SansSerif-Bold",
    "KaTeX_SansSerif-Italic",
    "KaTeX_SansSerif-Regular",
    "KaTeX_Script-Regular",
    "KaTeX_Size1-Regular",
    "KaTeX_Size2-Regular",
    "KaTeX_Size3-Regular",
    "KaTeX_Size4-Regular",
    "KaTeX_Typewriter-Regular",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(output_dir) = args.get(1) else {
        eprintln!("usage: canvas-vendor-fetch <resources/vendor dir>");
        std::process::exit(2);
    };
    let out = Path::new(output_dir);
    if let Err(e) = fetch_all(out) {
        eprintln!("canvas-vendor-fetch failed: {e:#}");
        std::process::exit(1);
    }
}

/// The served path for a library row: `<library dir>/<file name>`.
///
/// three.js's addons are the one exception to the flat layout: upstream
/// serves them from `examples/jsm/…` and they import each other by
/// relative path, so the subtree is preserved verbatim under
/// `three/addons/` — the same mount point the canonical import map
/// aliases as `three/addons/`.
fn served_for(package: &str, file: &str) -> String {
    let dir = LIBRARY_DIRS
        .iter()
        .find_map(|(pkg, dir)| (*pkg == package).then_some(*dir))
        .unwrap_or_else(|| panic!("package {package} needs a LIBRARY_DIRS entry"));
    if let Some(rest) = file.strip_prefix("examples/jsm/") {
        return format!("{dir}/addons/{rest}");
    }
    let name = file.rsplit('/').next().unwrap_or(file);
    format!("{dir}/{name}")
}

fn fetch_all(out: &Path) -> Result<(), String> {
    let curl = which_curl()?;
    let mut rows: Vec<String> = Vec::new();

    for (package, version, file) in LIBRARIES {
        let src = VendorSource {
            served: served_for(package, file),
            package,
            version,
            file: (*file).to_owned(),
        };
        let raw = download(&curl, &src)?;
        let gz = gzip_deterministic(&raw);
        write_asset(out, &src.served, &gz)?;
        rows.push(manifest_row(&src, &gz, "gzip"));
    }
    for (package, version, file, served) in LICENSES {
        let src = VendorSource {
            served: (*served).to_owned(),
            package,
            version,
            file: (*file).to_owned(),
        };
        let raw = download(&curl, &src)?;
        write_asset(out, &src.served, &raw)?;
        rows.push(manifest_row(&src, &raw, "identity"));
    }
    for font in KATEX_FONTS {
        let src = VendorSource {
            served: format!("katex/fonts/{font}.woff2"),
            package: "katex",
            version: "0.19.1",
            file: format!("dist/fonts/{font}.woff2"),
        };
        let raw = download(&curl, &src)?;
        write_asset(out, &src.served, &raw)?;
        rows.push(manifest_row(&src, &raw, "identity"));
    }

    rows.sort();
    write_manifest(out, &rows)?;
    println!("vendored {} files", rows.len());
    Ok(())
}

/// Locate a usable curl. The fetcher is a dev tool with no HTTP client
/// dependency; curl is the one prerequisite the recipe documents.
fn which_curl() -> Result<String, String> {
    let status = Command::new("curl").arg("--version").output();
    match status {
        Ok(out) if out.status.success() => Ok(String::from("curl")),
        _ => Err(String::from(
            "curl is required by canvas-vendor-update but was not found on PATH",
        )),
    }
}

fn download(curl: &str, src: &VendorSource) -> Result<Vec<u8>, String> {
    let url = format!(
        "https://cdn.jsdelivr.net/npm/{}@{}/{}",
        src.package, src.version, src.file
    );
    let output = Command::new(curl)
        .args(["-sfL", "--max-time", "120", &url])
        .output()
        .map_err(|e| format!("spawning curl for {url}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "download failed ({url}): curl exit {:?}",
            output.status.code()
        ));
    }
    if output.stdout.is_empty() {
        return Err(format!("download produced no bytes ({url})"));
    }
    Ok(output.stdout)
}

/// Deterministic gzip: level 9, fixed mtime, no embedded filename. The
/// committed bytes change only when the upstream file does.
fn gzip_deterministic(raw: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(raw)
        .expect("writing into a Vec encoder cannot fail");
    encoder
        .finish()
        .expect("finishing a Vec encoder cannot fail")
}

fn write_asset(out: &Path, served: &str, bytes: &[u8]) -> Result<(), String> {
    let path = out.join(served);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn manifest_row(src: &VendorSource, bytes: &[u8], encoding: &str) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "[[assets]]\nserved = \"{}\"\npackage = \"{}\"\nversion = \"{}\"\nsha256 = \"{}\"\nbytes = {}\nencoding = \"{}\"\n",
        src.served,
        src.package,
        src.version,
        hex,
        bytes.len(),
        encoding
    )
}

fn write_manifest(out: &Path, rows: &[String]) -> Result<(), String> {
    let mut doc = String::from(
        "# Canvas vendor manifest — generated by `just canvas-vendor-update`, never edited\n\
         # by hand. Each row pins one embedded asset: the served path, the npm package\n\
         # and exact version it came from, and the sha256 of the exact bytes the crate\n\
         # embeds (gzipped for the library dists, plain for licenses and fonts).\n\
         # The build-time hash test verifies every row against the embedded file.\n\n",
    );
    for row in rows {
        doc.push_str(row);
        doc.push('\n');
    }
    fs::write(out.join("vendor-manifest.toml"), doc)
        .map_err(|e| format!("writing vendor-manifest.toml: {e}"))
}
