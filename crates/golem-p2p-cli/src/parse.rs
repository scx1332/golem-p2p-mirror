//! Command line value parsers.

use std::time::Duration;

use golem_p2p::{
    iroh::{RelayConfig, RelayUrl},
    record::RELAY_URL_SCHEME,
};
use iroh_relay::RelayQuicConfig;

/// The QUIC address discovery (QAD) setting of a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuicPort {
    /// QAD on this UDP port of the relay host.
    Port(u16),
    /// No QAD with this relay.
    Off,
}

/// A `--relay` value: `URL[,quic=PORT|off]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySpec {
    pub url: RelayUrl,
    /// Per-relay QAD setting; `None` means "use `--relay-quic-port`".
    pub quic: Option<QuicPort>,
}

impl RelaySpec {
    /// The iroh relay configuration, with `default` as the QAD setting for a
    /// relay without its own. Without either, iroh's default port (7842).
    pub fn config(&self, default: Option<QuicPort>) -> RelayConfig {
        match self.quic.or(default) {
            None => RelayConfig::from(self.url.clone()),
            Some(QuicPort::Port(p)) => {
                RelayConfig::new(self.url.clone(), Some(RelayQuicConfig::new(p)))
            }
            Some(QuicPort::Off) => RelayConfig::new(self.url.clone(), None),
        }
    }
}

/// Parses a relay URL and rejects every scheme except `https`.
pub fn relay_url(s: &str) -> Result<RelayUrl, String> {
    let url: RelayUrl = s.parse().map_err(|e| format!("{e}"))?;
    if url.scheme() != RELAY_URL_SCHEME {
        return Err(format!(
            "relay URLs must use https, got {}://",
            url.scheme()
        ));
    }
    Ok(url)
}

/// Parses `URL[,quic=PORT|off]`.
pub fn relay_spec(s: &str) -> Result<RelaySpec, String> {
    let mut parts = s.split(',');
    let url = relay_url(parts.next().unwrap_or_default())?;
    let mut quic = None;
    for opt in parts {
        match opt.split_once('=') {
            Some(("quic", v)) => quic = Some(quic_port(v)?),
            _ => {
                return Err(format!(
                    "unknown relay option {opt:?} (expected quic=PORT or quic=off)"
                ));
            }
        }
    }
    Ok(RelaySpec { url, quic })
}

/// Parses a QAD port: a UDP port number (1-65535) or `off`.
pub fn quic_port(s: &str) -> Result<QuicPort, String> {
    if s.eq_ignore_ascii_case("off") {
        return Ok(QuicPort::Off);
    }
    match s.parse::<u16>() {
        Ok(p) if p > 0 => Ok(QuicPort::Port(p)),
        _ => Err(format!("invalid QUIC port {s:?} (expected 1-65535 or off)")),
    }
}

/// Parses a duration: a number of seconds (`30`, `1.5`) or a number with a
/// unit `ms`, `s`, `m` or `h` (`250ms`, `30s`, `2m`).
pub fn duration(s: &str) -> Result<Duration, String> {
    let (num, unit) = split_number(s);
    let factor = match unit {
        "" | "s" => 1.0,
        "ms" => 0.001,
        "m" => 60.0,
        "h" => 3600.0,
        _ => return Err(format!("invalid duration {s:?} (units: ms, s, m, h)")),
    };
    let v: f64 = num.parse().map_err(|_| format!("invalid duration {s:?}"))?;
    Duration::try_from_secs_f64(v * factor).map_err(|_| format!("invalid duration {s:?}"))
}

/// Parses a byte count: a number with an optional unit, decimal (`k`/`kB`,
/// `M`/`MB`, `G`/`GB`) or binary (`Ki`/`KiB`, `Mi`/`MiB`, `Gi`/`GiB`).
pub fn bytes(s: &str) -> Result<u64, String> {
    let (num, unit) = split_number(s);
    let factor: u64 = match unit {
        "" | "B" => 1,
        "k" | "K" | "kB" | "KB" => 1_000,
        "M" | "MB" => 1_000_000,
        "G" | "GB" => 1_000_000_000,
        "Ki" | "KiB" => 1 << 10,
        "Mi" | "MiB" => 1 << 20,
        "Gi" | "GiB" => 1 << 30,
        _ => return Err(format!("invalid size {s:?} (units: k, M, G, Ki, Mi, Gi)")),
    };
    if let Ok(n) = num.parse::<u64>() {
        return n
            .checked_mul(factor)
            .ok_or_else(|| format!("size {s:?} too large"));
    }
    let v: f64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    let n = (v * factor as f64).round();
    if !(0.0..=u64::MAX as f64).contains(&n) {
        return Err(format!("invalid size {s:?}"));
    }
    Ok(n as u64)
}

/// Splits `"1.5ms"` into `("1.5", "ms")`.
fn split_number(s: &str) -> (&str, &str) {
    let s = s.trim();
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    (&s[..end], s[end..].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_specs() {
        let s = relay_spec("https://relay.example.org:56000").unwrap();
        assert_eq!(s.url.as_str(), "https://relay.example.org:56000/");
        assert_eq!(s.quic, None);
        let s = relay_spec("https://1.2.3.4:56000,quic=56001").unwrap();
        assert_eq!(s.quic, Some(QuicPort::Port(56001)));
        let s = relay_spec("https://1.2.3.4:56000,quic=off").unwrap();
        assert_eq!(s.quic, Some(QuicPort::Off));

        for bad in [
            "http://1.2.3.4:3340",
            "http://1.2.3.4:3340,quic=7842",
            "https://1.2.3.4,quic=0",
            "https://1.2.3.4,quic=70000",
            "https://1.2.3.4,qad=7842",
            "https://1.2.3.4,",
        ] {
            assert!(relay_spec(bad).is_err(), "{bad}");
        }
        assert!(
            relay_spec("http://1.2.3.4")
                .unwrap_err()
                .contains("relay URLs must use https")
        );
    }

    #[test]
    fn relay_configs() {
        let default_port = iroh_relay::defaults::DEFAULT_RELAY_QUIC_PORT;
        let port = |cfg: RelayConfig| cfg.quic.map(|q| q.port);
        let plain = relay_spec("https://1.2.3.4:56000").unwrap();
        let own = relay_spec("https://1.2.3.4:56000,quic=56001").unwrap();
        let off = relay_spec("https://1.2.3.4:56000,quic=off").unwrap();

        assert_eq!(port(plain.config(None)), Some(default_port));
        assert_eq!(port(plain.config(Some(QuicPort::Port(9)))), Some(9));
        assert_eq!(port(plain.config(Some(QuicPort::Off))), None);
        // A per-relay setting wins over `--relay-quic-port`.
        assert_eq!(port(own.config(None)), Some(56001));
        assert_eq!(port(own.config(Some(QuicPort::Off))), Some(56001));
        assert_eq!(port(off.config(Some(QuicPort::Port(9)))), None);
        assert_eq!(own.config(None).url, own.url);
    }

    #[test]
    fn quic_ports() {
        assert_eq!(quic_port("56001"), Ok(QuicPort::Port(56001)));
        assert_eq!(quic_port("OFF"), Ok(QuicPort::Off));
        assert!(quic_port("0").is_err());
        assert!(quic_port("-1").is_err());
        assert!(quic_port("").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(duration("30"), Ok(Duration::from_secs(30)));
        assert_eq!(duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(duration("1.5"), Ok(Duration::from_millis(1500)));
        assert_eq!(duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(duration("1h"), Ok(Duration::from_secs(3600)));
        for bad in ["", "s", "-1", "1x", "1.2.3", "inf"] {
            assert!(duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(bytes("0"), Ok(0));
        assert_eq!(bytes("100"), Ok(100));
        assert_eq!(bytes("4M"), Ok(4_000_000));
        assert_eq!(bytes("4MB"), Ok(4_000_000));
        assert_eq!(bytes("1.5k"), Ok(1500));
        assert_eq!(bytes("2MiB"), Ok(2 << 20));
        assert_eq!(bytes("1Gi"), Ok(1 << 30));
        for bad in ["", "M", "-1", "1x", "99999999999999999999G"] {
            assert!(bytes(bad).is_err(), "{bad}");
        }
    }
}
