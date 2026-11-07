//! Part A of `bench`: what can be learned about the local network without a
//! peer.
//!
//! * iroh's net report (`iroh::Endpoint::net_report`, feature
//!   `unstable-net-report`): UDP over IPv4/IPv6 (QUIC address discovery to
//!   the relays), the public addresses the relays saw, whether they differ
//!   between relays (hard NAT), per-relay latencies, the preferred relay.
//! * Our own direct addresses as published in our record, and the home
//!   relay (`home_relay_status`).
//! * Each configured relay: DNS, TCP, TLS with the configured trust roots,
//!   and `GET /ping`, each step timed.
//! * Each configured directory: a lookup of our own node ID, to see whether
//!   our record was published.

use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use golem_p2p::{Discovery, Endpoint, HttpDirectory, iroh::Watcher};
use iroh::{
    RelayUrl,
    unstable_net_report::{NetReport as IrohReport, Probe},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{CertificateError, ClientConfig, Error as TlsError, pki_types::ServerName},
};

use super::report::{DirectoryCheck, HomeRelay, Local, NetReport, RelayCheck};
use crate::{NodeArgs, out::ms};

/// Time limit for each relay and directory check.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs the local network check.
pub async fn check(ep: &Endpoint, args: &NodeArgs) -> Local {
    let start = Instant::now();
    let mut tls = ep.iroh().tls_config().clone();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tls = Arc::new(tls);
    let relays = args.relay.iter().map(|r| {
        let cfg = r.config(args.relay_quic_port);
        check_relay(cfg.url.clone(), cfg.quic.map(|q| q.port), tls.clone())
    });
    let dirs = args
        .directory
        .iter()
        .map(|d| check_directory(d.clone(), ep));
    let (report, mut relays, directories) = tokio::join!(
        wait_for_net_report(ep, !args.relay.is_empty()),
        n0_future::join_all(relays),
        n0_future::join_all(dirs),
    );

    if let Some(r) = &report {
        add_latencies(r, &mut relays);
    }
    // Without UDP sockets (--relay-only) the report has no UDP results.
    let net_report = report
        .as_ref()
        .filter(|_| !args.relay_only)
        .map(|r| convert(r, ep));
    let home_relay = ep
        .iroh()
        .home_relay_status()
        .get()
        .first()
        .map(|h| HomeRelay {
            url: Some(h.url().to_string()),
            connected: h.is_connected(),
            error: h.last_error().map(|e| e.to_string()),
        });
    let answered: Vec<_> = directories.iter().filter(|d| d.ok).collect();
    let record_published =
        (!answered.is_empty()).then(|| answered.iter().any(|d| d.own_record == Some("published")));
    Local {
        duration_ms: ms(start.elapsed()),
        bound_sockets: ep
            .iroh()
            .bound_sockets()
            .iter()
            .map(|a| a.to_string())
            .collect(),
        direct_addrs: ep
            .local_record()
            .record()
            .direct_addrs
            .iter()
            .map(|a| a.to_string())
            .collect(),
        home_relay: home_relay.or_else(|| {
            (!args.relay.is_empty()).then_some(HomeRelay {
                url: None,
                connected: false,
                error: None,
            })
        }),
        net_report,
        relays,
        directories,
        record_published,
    }
}

/// iroh's latest net report; waits for the first one (at most a little more
/// than iroh's own report timeout). None without relays.
async fn wait_for_net_report(ep: &Endpoint, have_relays: bool) -> Option<IrohReport> {
    if !have_relays {
        return None;
    }
    let mut w = ep.iroh().net_report();
    let limit = Duration::from_secs(golem_p2p::iroh::NET_REPORT_TIMEOUT + 1);
    tokio::time::timeout(limit, w.initialized()).await.ok()
}

/// Copies the per-relay latencies of the net report into the relay checks.
fn add_latencies(r: &IrohReport, relays: &mut [RelayCheck]) {
    for (probe, url, latency) in r.relay_latency.iter() {
        let Some(c) = relays.iter_mut().find(|c| c.url == url.to_string()) else {
            continue;
        };
        let v = Some(ms(latency));
        match probe {
            Probe::Https => c.https_latency_ms = v,
            Probe::QadIpv4 => c.qad_v4_ms = v,
            Probe::QadIpv6 => c.qad_v6_ms = v,
            _ => {}
        }
    }
}

fn convert(r: &IrohReport, ep: &Endpoint) -> NetReport {
    let mut v4 = BTreeSet::<&RelayUrl>::new();
    let mut v6 = BTreeSet::<&RelayUrl>::new();
    for (probe, url, _) in r.relay_latency.iter() {
        match probe {
            Probe::QadIpv4 => {
                v4.insert(url);
            }
            Probe::QadIpv6 => {
                v6.insert(url);
            }
            _ => {}
        }
    }
    // iroh compares the public addresses seen by different relays. With
    // answers from fewer than two relays the comparison says nothing, even
    // if iroh's flag is set (repeated answers from the same relay).
    let varies4 = (v4.len() >= 2)
        .then_some(r.mapping_varies_by_dest_ipv4)
        .flatten();
    let varies6 = (v6.len() >= 2)
        .then_some(r.mapping_varies_by_dest_ipv6)
        .flatten();
    let mapping_varies_by_dest = match (varies4, varies6) {
        (Some(a), Some(b)) => Some(a || b),
        (a, b) => a.or(b),
    };
    let local_v4_port = ep
        .iroh()
        .bound_sockets()
        .iter()
        .find(|a| a.is_ipv4() && a.port() != 0)
        .map(|a| a.port());
    NetReport {
        udp_v4: r.udp_v4,
        udp_v6: r.udp_v6,
        public_v4: r.global_v4.map(|a| a.to_string()),
        public_v6: r.global_v6.map(|a| a.to_string()),
        behind_nat_v4: r.global_v4.and_then(|g| {
            let local = local_source_ip(SocketAddr::V4(g))?;
            Some(local != IpAddr::V4(*g.ip()))
        }),
        port_preserved_v4: r
            .global_v4
            .and_then(|g| local_v4_port.map(|p| p == g.port())),
        mapping_varies_by_dest,
        qad_relays_v4: v4.len(),
        qad_relays_v6: v6.len(),
        preferred_relay: r.preferred_relay.as_ref().map(|u| u.to_string()),
        captive_portal: r.captive_portal,
    }
}

/// The local address the OS would send from to reach `dest`. No packet is
/// sent: a UDP socket is only connected.
fn local_source_ip(dest: SocketAddr) -> Option<IpAddr> {
    let bind: SocketAddr = if dest.is_ipv4() {
        "0.0.0.0:0".parse().ok()?
    } else {
        "[::]:0".parse().ok()?
    };
    let sock = std::net::UdpSocket::bind(bind).ok()?;
    sock.connect(dest).ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Checks one relay: DNS, TCP, TLS (with the endpoint's trust roots) and
/// `GET /ping`.
async fn check_relay(url: RelayUrl, quic_port: Option<u16>, tls: Arc<ClientConfig>) -> RelayCheck {
    let mut c = RelayCheck {
        url: url.to_string(),
        quic_port,
        ..Default::default()
    };
    let mut stage = "dns";
    let res = tokio::time::timeout(CHECK_TIMEOUT, relay_steps(&url, tls, &mut c, &mut stage)).await;
    match res {
        Ok(Ok(())) => c.ok = true,
        Ok(Err((kind, msg))) => {
            c.error_kind = Some(kind);
            c.error = Some(msg);
        }
        Err(_) => {
            let what = match stage {
                "dns" => "resolving the host name",
                "tcp" => "connecting over TCP",
                "tls" => "the TLS handshake",
                _ => "the HTTPS request",
            };
            c.error_kind = Some("timeout");
            c.error = Some(format!(
                "no answer within {} s ({what})",
                CHECK_TIMEOUT.as_secs()
            ));
        }
    }
    c
}

type StepError = (&'static str, String);

async fn relay_steps(
    url: &RelayUrl,
    tls: Arc<ClientConfig>,
    c: &mut RelayCheck,
    stage: &mut &'static str,
) -> Result<(), StepError> {
    let host = url
        .host_str()
        .ok_or(("dns", "relay URL has no host".to_string()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let (addrs, name): (Vec<SocketAddr>, ServerName<'static>) = match url.host() {
        Some(url::Host::Ipv4(ip)) => (vec![(ip, port).into()], ServerName::IpAddress(ip.into())),
        Some(url::Host::Ipv6(ip)) => (vec![(ip, port).into()], ServerName::IpAddress(ip.into())),
        _ => {
            let t = Instant::now();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|e| ("dns", format!("cannot resolve {host}: {e}")))?
                .collect();
            c.dns_ms = Some(ms(t.elapsed()));
            if addrs.is_empty() {
                return Err(("dns", format!("{host} has no address")));
            }
            let name = ServerName::try_from(host.to_string())
                .map_err(|e| ("dns", format!("invalid host name {host}: {e}")))?;
            (addrs, name)
        }
    };

    *stage = "tcp";
    let t = Instant::now();
    let mut tcp = None;
    let mut errors = Vec::new();
    for a in &addrs {
        match TcpStream::connect(a).await {
            Ok(s) => {
                c.addr = Some(a.to_string());
                tcp = Some(s);
                break;
            }
            Err(e) => errors.push(format!("{a}: {e}")),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        (
            "tcp",
            format!("cannot connect over TCP ({})", errors.join("; ")),
        )
    })?;
    c.tcp_ms = Some(ms(t.elapsed()));

    *stage = "tls";
    let t = Instant::now();
    let mut stream = TlsConnector::from(tls)
        .connect(name, tcp)
        .await
        .map_err(tls_error)?;
    c.tls_ms = Some(ms(t.elapsed()));

    *stage = "http";
    let t = Instant::now();
    let req = format!(
        "GET /ping HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: golem-p2p-bench\r\n\
         Connection: close\r\n\r\n"
    );
    let http = |e: std::io::Error| ("http", format!("HTTPS request failed: {e}"));
    stream.write_all(req.as_bytes()).await.map_err(http)?;
    let mut buf = vec![0u8; 1024];
    let mut n = 0;
    while n < buf.len() && !buf[..n].windows(2).any(|w| w == b"\r\n") {
        let k = stream.read(&mut buf[n..]).await.map_err(http)?;
        if k == 0 {
            break;
        }
        n += k;
    }
    c.http_ms = Some(ms(t.elapsed()));
    let status_line = String::from_utf8_lossy(&buf[..n]);
    let status: Option<u16> = status_line
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok());
    c.http_status = status;
    match status {
        Some(s) if (200..300).contains(&s) => Ok(()),
        Some(s) => Err(("http", format!("GET /ping answered with HTTP status {s}"))),
        None => Err(("http", "no HTTP answer to GET /ping".to_string())),
    }
}

fn tls_error(e: std::io::Error) -> StepError {
    let inner = e.get_ref().and_then(|i| i.downcast_ref::<TlsError>());
    match inner {
        Some(TlsError::InvalidCertificate(CertificateError::UnknownIssuer)) => (
            "tls_untrusted",
            "the relay's TLS certificate is not trusted (unknown issuer)".to_string(),
        ),
        Some(TlsError::InvalidCertificate(
            CertificateError::Expired | CertificateError::ExpiredContext { .. },
        )) => (
            "tls_certificate",
            "the relay's TLS certificate has expired".to_string(),
        ),
        Some(TlsError::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
        )) => (
            "tls_certificate",
            "the relay's TLS certificate is not valid for its address".to_string(),
        ),
        Some(TlsError::InvalidCertificate(other)) => (
            "tls_certificate",
            format!("invalid relay TLS certificate: {other:?}"),
        ),
        Some(other) => ("tls", format!("TLS handshake failed: {other}")),
        None => ("tls", format!("TLS handshake failed: {e}")),
    }
}

/// Looks up our own node ID in one directory.
async fn check_directory(url: url::Url, ep: &Endpoint) -> DirectoryCheck {
    let mut c = DirectoryCheck {
        url: url.to_string(),
        ..Default::default()
    };
    let dir = match HttpDirectory::new(url) {
        Ok(d) => d,
        Err(e) => {
            c.error = Some(e.to_string());
            return c;
        }
    };
    let t = Instant::now();
    match tokio::time::timeout(CHECK_TIMEOUT, dir.lookup(ep.node_id())).await {
        Ok(Ok(rec)) => {
            c.ok = true;
            c.lookup_ms = Some(ms(t.elapsed()));
            c.own_record = Some(match rec {
                Some(r) if r.endpoint_id() == ep.endpoint_id() => "published",
                Some(_) => "other_endpoint",
                None => "missing",
            });
        }
        Ok(Err(e)) => c.error = Some(e.to_string()),
        Err(_) => c.error = Some(format!("no answer within {} s", CHECK_TIMEOUT.as_secs())),
    }
    c
}
