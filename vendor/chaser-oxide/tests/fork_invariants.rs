//! Fork-invariant tests: stealth properties jinn depends on.
//!
//! These are pure unit tests — no browser launches, no network. They assert
//! the two structural invariants the jinn browser daemon is built on:
//! the frame-init command chain never enables the Runtime or Network domains,
//! and the default launch args carry no automation-telling flags.

use chaser_oxide::BrowserConfig;
use chaser_oxide::handler::frame::FrameManager;

/// The method identifiers the init chain must never contain.
const FORBIDDEN_INIT_METHODS: [&str; 2] = ["Runtime.enable", "Network.enable"];

#[test]
fn the_upstream_default_viewport_is_the_800x600_pin() {
    // The upstream `BrowserConfig::default` sets
    // `viewport: Some(Viewport::default())` — a hardcoded 800x600 —
    // which fires `Emulation.setDeviceMetricsOverride` on every target
    // init and pins the page renderer to an 800x600 box inside the real
    // window (observed live: outerWidth 3840, innerWidth 800). An
    // 800x600 viewport on a 4K display is itself a bot signal, so jinn
    // disables the emulation (`viewport(None)`) and lets the page fill
    // the window. This test pins the upstream default the choice works
    // around: if Viewport::default() ever stops being 800x600, this
    // assertion breaks and the fork-side decision should be revisited.
    let viewport = chaser_oxide::handler::viewport::Viewport::default();
    assert_eq!(
        (viewport.width, viewport.height),
        (800, 600),
        "upstream default viewport changed; re-evaluate the daemon's viewport(None)"
    );
}

#[test]
fn init_sends_no_runtime_or_network_enable() {
    // Given the frame manager's init command chain (the per-target init
    // sequence every page target runs on attach).
    let mut chain = FrameManager::init_commands(std::time::Duration::from_secs(5));

    // When draining every command identifier the chain will send.
    let mut methods = Vec::new();
    let now = std::time::Instant::now();
    while let std::task::Poll::Ready(Some(Ok((method, _)))) = chain.poll(now) {
        methods.push(method.to_string());
    }

    // Then no CDP-detection signal appears: neither Runtime.enable (the
    // rebrowser C1/C2 leak) nor Network.enable (the C3 leak) is sent.
    for forbidden in FORBIDDEN_INIT_METHODS {
        assert!(
            !methods.iter().any(|m| m == forbidden),
            "init chain must not contain {forbidden}; found {methods:?}"
        );
    }
}

#[test]
fn launch_args_exclude_automation_tells() {
    // Given the default browser config.
    let config = BrowserConfig::builder().build().unwrap();

    // When rendering the args the child will receive.
    let args: Vec<String> = config.build_args(&["about:blank".to_owned()]);

    // Then no automation-telling flag survives: dropped upstream
    // (--enable-automation) and dropped by this fork (F1/V1).
    let forbidden = [
        "--enable-automation",
        "--disable-ipc-flooding-protection",
        "--disable-component-extensions-with-background-pages",
    ];
    for flag in forbidden {
        assert!(
            !args.iter().any(|a| a == flag),
            "launch args must not contain {flag}; got {args:?}"
        );
    }

    // And the automation-controlled blink feature stays hidden.
    assert!(
        args.iter()
            .any(|a| a.contains("disable-blink-features=AutomationControlled")),
        "expected the AutomationControlled hiding flag; got {args:?}"
    );

    // And the native-wayland occlusion tracker stays off: an occlusion
    // verdict frame-throttles Input.dispatchMouseEvent acks to one per
    // 5s, which is the input-wedge signature.
    assert!(
        args.iter().any(|a| {
            a.contains("disable-features") && a.contains("CalculateNativeWinOcclusion")
        }),
        "expected CalculateNativeWinOcclusion disabled; got {args:?}"
    );
}
