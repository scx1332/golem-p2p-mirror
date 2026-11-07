//! Command line value parsers.

use std::time::Duration;

use golem_p2p::{iroh::RelayUrl, record::RELAY_URL_SCHEME};

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
