//! Output of the CLI: human-readable text, or one JSON object per line.
//!
//! With `--json`, stdout carries only events (see `docs/cli-events.md`), one
//! per line, flushed after every line. Logs always go to stderr.

use std::{
    fmt::Write as _,
    io::Write as _,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use golem_p2p::PathKind;
use serde_json::Value;

/// Where events go. Cheap to clone.
#[derive(Clone)]
pub struct Out {
    inner: Arc<OutInner>,
}

struct OutInner {
    json: bool,
    run: Value,
    node: Value,
    /// Set after the last event of a command (`summary`), so that late events
    /// from background tasks never follow it. Also serializes writers.
    sealed: Mutex<bool>,
}

impl Out {
    pub fn new(json: bool, run: Option<String>, node: Option<String>) -> Self {
        Self {
            inner: Arc::new(OutInner {
                json,
                run: run.into(),
                node: node.into(),
                sealed: Mutex::new(false),
            }),
        }
    }

    /// `true` if events are printed as JSON lines.
    pub fn json(&self) -> bool {
        self.inner.json
    }

    /// Starts an event. Without `--json` it is discarded on `emit`.
    pub fn event(&self, name: &str) -> Ev<'_> {
        let buf = self.inner.json.then(|| {
            let mut buf = String::with_capacity(256);
            buf.push_str("{\"ts\":");
            push_json(&mut buf, &Value::from(rfc3339_ms(SystemTime::now())));
            buf.push_str(",\"event\":");
            push_json(&mut buf, &Value::from(name));
            buf.push_str(",\"run\":");
            push_json(&mut buf, &self.inner.run);
            buf.push_str(",\"node\":");
            push_json(&mut buf, &self.inner.node);
            buf
        });
        Ev { out: self, buf }
    }

    fn write(&self, line: &str, last: bool) {
        let mut sealed = self.inner.sealed.lock().expect("poisoned");
        if *sealed {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        // A closed stdout (the controller went away) must not kill the node.
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
        *sealed = last;
    }
}

/// An event under construction. Fields keep their insertion order.
#[must_use = "an event is only printed by `emit`"]
pub struct Ev<'a> {
    out: &'a Out,
    buf: Option<String>,
}

impl Ev<'_> {
    /// Adds a field. Keys are fixed names from `docs/cli-events.md`.
    pub fn field(mut self, key: &str, value: impl Into<Value>) -> Self {
        if let Some(buf) = &mut self.buf {
            buf.push(',');
            push_json(buf, &Value::from(key));
            buf.push(':');
            push_json(buf, &value.into());
        }
        self
    }

    /// Prints the event (JSON mode only).
    pub fn emit(self) {
        self.finish(false);
    }

    /// Prints the event and suppresses every later event.
    pub fn emit_last(self) {
        self.finish(true);
    }

    fn finish(self, last: bool) {
        if let Some(mut buf) = self.buf {
            buf.push('}');
            self.out.write(&buf, last);
        }
    }
}

fn push_json(buf: &mut String, v: &Value) {
    // Serializing a `Value` cannot fail.
    let _ = write!(buf, "{v}");
}

/// Formats a time as RFC 3339 in UTC with milliseconds,
/// e.g. `2026-11-07T15:09:12.345Z`.
pub fn rfc3339_ms(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (y, m, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian
/// calendar (H. Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Milliseconds as a float with microsecond precision.
pub fn ms(d: Duration) -> f64 {
    round3(d.as_secs_f64() * 1000.0)
}

/// Rounds to three decimals.
pub fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The name of a path kind in events and human output.
pub fn path_name(p: PathKind) -> &'static str {
    match p {
        PathKind::Direct => "direct",
        PathKind::Relay => "relay",
        PathKind::Unknown => "unknown",
    }
}

/// A stable, machine-readable kind for a library error.
pub fn error_kind(e: &golem_p2p::Error) -> &'static str {
    use golem_p2p::Error as E;
    match e {
        E::NotFound(_) => "not_found",
        E::NotLocalIdentity(_) | E::SelfConnect(_) | E::InvalidIdentities(_) => "identity",
        E::NotPeerIdentity(_) => "not_peer_identity",
        E::Record(_) => "record",
        E::Discovery(_) => "discovery",
        E::InsecureRelayUrl(_) => "config",
        E::Bind(_) => "bind",
        E::Connect(_) => "connect",
        E::Handshake(_) | E::IdentityMismatch => "handshake",
        E::Rejected(_) => "rejected",
        E::StreamRejected(_) => "stream_rejected",
        E::Connection(_) => "connection",
        E::Frame(_) => "protocol",
        E::TooLarge => "too_large",
        E::Timeout => "timeout",
        E::Closed => "closed",
    }
}

/// The error kind of an `anyhow` error: the kind of the first library error
/// in its chain, or `fallback`.
pub fn anyhow_kind(e: &anyhow::Error, fallback: &'static str) -> &'static str {
    e.chain()
        .find_map(|c| c.downcast_ref::<golem_p2p::Error>())
        .map(error_kind)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64, millis: u64) -> String {
        rfc3339_ms(UNIX_EPOCH + Duration::from_millis(secs * 1000 + millis))
    }

    #[test]
    fn timestamps() {
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(at(951_782_400, 7), "2000-02-29T00:00:00.007Z");
        assert_eq!(at(951_868_799, 999), "2000-02-29T23:59:59.999Z");
        assert_eq!(at(1_700_000_000, 123), "2023-11-14T22:13:20.123Z");
        assert_eq!(at(4_107_542_400, 0), "2100-03-01T00:00:00.000Z");
        assert_eq!(at(1_798_761_599, 500), "2026-12-31T23:59:59.500Z");
    }

    #[test]
    fn rounding() {
        assert_eq!(ms(Duration::from_micros(74_123)), 74.123);
        assert_eq!(ms(Duration::from_nanos(1_500_600)), 1.501);
        assert_eq!(round3(0.12345), 0.123);
    }
}
