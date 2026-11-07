//! The CLI accepts only https relay URLs.

use std::process::{Command, Output};

fn listen_with_relay(relay: &str) -> Output {
    // The key file does not exist: argument parsing happens before it is read,
    // so the error tells which of the two checks failed.
    Command::new(env!("CARGO_BIN_EXE_golem-p2p"))
        .args([
            "listen",
            "--key",
            "/nonexistent/golem-p2p-test.key",
            "--directory",
            "http://127.0.0.1:9/",
            "--relay",
            relay,
        ])
        .output()
        .expect("run golem-p2p")
}

#[test]
fn non_https_relay_urls_rejected() {
    for relay in [
        "http://127.0.0.1:3340",
        "HTTP://relay.example.org",
        "ws://127.0.0.1:3340",
    ] {
        let out = listen_with_relay(relay);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{relay}: {stderr}");
        assert!(
            stderr.contains("relay URLs must use https"),
            "{relay}: {stderr}"
        );
    }
}

#[test]
fn https_relay_url_accepted() {
    // Parsing succeeds and the command fails later, on the missing key file.
    // The directory URL may still be plain http.
    let out = listen_with_relay("https://127.0.0.1:3340");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("relay"), "{stderr}");
    assert!(stderr.contains("golem-p2p-test.key"), "{stderr}");
}
