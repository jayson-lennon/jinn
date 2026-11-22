//! Manual spike (not a test): does `get_page` work on an OOPIF iframe
//! target through the chaser layer?
//!
//! Run: cargo build --workspace --examples && target/debug/examples/oopif_spike
//! Launches its OWN chromium on a scratch profile (never touches the
//! daemon's live browser), navigates to BBC, finds the Sourcepoint iframe
//! target, evaluates + clicks inside it, and reports whether the banner
//! disappeared from the parent page.
//!
//! Cleanup discipline: the shutdown guard runs on EVERY exit path —
//! success, error, and panic — so a cancelled or failing run never
//! leaks a chromium instance.
#![allow(
    clippy::too_many_lines,
    clippy::print_stdout,
    clippy::use_debug,
    reason = "manual spike harness; stdout is the harness's report, and debug formats name the values"
)]

use error_stack::Report;
use wherror::Error;

#[derive(Debug, Error)]
#[error(debug)]
enum SpikeError {
    #[error("no browser binary found")]
    NoBinary,
    #[error("launch failed: {0}")]
    Launch(String),
    #[error("page command failed: {0}")]
    Page(String),
    #[error("target enumeration failed: {0}")]
    Targets(String),
    #[error("{0}")]
    Other(String),
}

fn main() -> Result<(), Report<SpikeError>> {
    // reqwest is built `rustls-no-provider`; install ring like the
    // jinn-mcp transport tests do (errors on second call are ignored).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Report::new(SpikeError::Other(e.to_string())))?;
    runtime.block_on(run()).map_err(|e| Report::new(e))
}

/// Kills any chromium still alive from this spike, by name, as the
/// backstop behind the guard's graceful shutdown.
fn backstop_kill() {
    let _ = std::process::Command::new("killall")
        .args(["-9", "chromium"])
        .output();
}

/// Graceful shutdown + backstop on every exit path, panic included.
/// Lives at module scope so `run` reads as a straight recipe.
struct Guard(Box<dyn jinn_browser::backend::EngineHandle>);
impl Drop for Guard {
    fn drop(&mut self) {
        let inner = &mut self.0;
        let _ = futures::executor::block_on(inner.shutdown());
        backstop_kill();
        println!("[guard] chromium shut down");
    }
}

#[cfg(feature = "daemon")]
async fn run() -> Result<(), SpikeError> {
    use jinn_browser::backend::{ChaserLauncher, EngineLauncher};
    use std::time::Duration;

    // Resolve the same way the daemon does: chromium first (user does not
    // use Chrome; any instance is ours).
    let probe = jinn_common::binary_detect::WhichProbe;
    let candidates: &[&str] = &["chromium", "chromium-browser", "google-chrome"];
    let executable = jinn_common::binary_detect::resolve_browser_binary(&probe, candidates)
        .ok_or(SpikeError::NoBinary)?
        .into_inner();

    println!("binary: {}", executable.display());

    let scratch = tempfile::env::temp_dir().join("oopif-spike-profile");
    let _ = std::fs::remove_dir_all(&scratch);

    println!("launching scratch chromium…");
    let launcher = ChaserLauncher;
    let live = launcher
        .launch(&executable, &scratch)
        .await
        .map_err(|e| SpikeError::Launch(e.to_string()))?;
    let handle = live.handle;
    let _death = live.death;

    let guard = Guard(handle);

    let (target_id, page) = guard
        .0
        .new_page("https://www.bbc.com/news".to_owned())
        .await
        .map_err(|e| SpikeError::Page(e.to_string()))?;
    println!("main tab target {target_id:?}; waiting for frames…");
    tokio::time::sleep(Duration::from_secs(9)).await;

    // ---- enumerate iframe targets via DevToolsActivePort /json ----
    let port_file = scratch.join("DevToolsActivePort");
    let port_line = std::fs::read_to_string(&port_file).map_err(|e| {
        SpikeError::Targets(format!(
            "no DevToolsActivePort in {}: {e}",
            scratch.display()
        ))
    })?;
    let port: u16 = port_line
        .lines()
        .next()
        .and_then(|l| l.parse().ok())
        .ok_or_else(|| SpikeError::Targets("empty DevToolsActivePort".to_owned()))?;
    let list: Vec<serde_json::Value> = reqwest::get(format!("http://127.0.0.1:{port}/json"))
        .await
        .map_err(|e| SpikeError::Targets(e.to_string()))?
        .json()
        .await
        .map_err(|e| SpikeError::Targets(e.to_string()))?;
    let mut found = None;
    for t in &list {
        let ty = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "iframe" {
            let head: String = url.chars().take(70).collect();
            println!("iframe target: {id} {head}");
            if url.contains("privacy-mgmt") {
                found = Some(id.to_owned());
            }
        }
    }
    let Some(iframe_id) = found else {
        return Err(SpikeError::Targets(
            "no Sourcepoint iframe target found".to_owned(),
        ));
    };
    println!("attaching chaser get_page to iframe target {iframe_id}");

    let iframe_target = chaser_oxide::cdp::browser_protocol::target::TargetId::from(iframe_id);
    let mut iframe_page = None;
    for attempt in 0..20 {
        match guard.0.get_page(&iframe_target).await {
            Ok(p) => {
                println!("get_page(iframe) succeeded on attempt {attempt}");
                iframe_page = Some(p);
                break;
            }
            Err(e) => {
                println!("get_page(iframe) attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let Some(iframe_page) = iframe_page else {
        return Err(SpikeError::Page(
            "get_page(iframe) never succeeded".to_owned(),
        ));
    };

    // evaluate inside the iframe
    let probe_out = iframe_page
        .evaluate_stealth("location.href".to_owned())
        .await
        .map_err(|e| SpikeError::Page(format!("eval in iframe: {e}")))?;
    println!("iframe evaluate(location.href) => {probe_out:?}");

    // find the accept button inside the iframe
    let button = iframe_page
        .evaluate_stealth(
            r#"(() => {
                const els = [...document.querySelectorAll('button, [role=button], a')];
                const b = els.find(x => /accept/i.test((x.textContent||'') + ' ' + (x.id||'')));
                if (!b) return 'no button';
                const r = b.getBoundingClientRect();
                return JSON.stringify({x: Math.round(r.x + r.width/2), y: Math.round(r.y + r.height/2), text: b.textContent.trim().slice(0,30)});
            })()"#
            .to_owned(),
        )
        .await
        .map_err(|e| SpikeError::Page(format!("button probe: {e}")))?;
    println!("button probe => {button:?}");

    let Some(serde_json::Value::String(encoded)) = &button else {
        return Err(SpikeError::Other(format!(
            "button probe returned non-string: {button:?}"
        )));
    };
    let v: serde_json::Value =
        serde_json::from_str(encoded).map_err(|e| SpikeError::Other(e.to_string()))?;
    let x = v.get("x").and_then(serde_json::Value::as_f64);
    let y = v.get("y").and_then(serde_json::Value::as_f64);
    let (Some(x), Some(y)) = (x, y) else {
        return Err(SpikeError::Other(format!(
            "button probe had no coords: {encoded}"
        )));
    };

    println!("trusted click inside iframe at ({x}, {y})…");
    iframe_page
        .click_human(x, y)
        .await
        .map_err(|e| SpikeError::Page(format!("iframe click: {e}")))?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let gone = page
        .evaluate_stealth("!document.querySelector('[id^=sp_message_]')".to_owned())
        .await
        .map_err(|e| SpikeError::Page(format!("parent check: {e}")))?;
    println!("BANNER GONE FROM PARENT => {gone:?}");

    Ok(())
}

#[cfg(not(feature = "daemon"))]
async fn run() -> Result<(), SpikeError> {
    Err(SpikeError::Other(
        "build with the daemon feature".to_owned(),
    ))
}
