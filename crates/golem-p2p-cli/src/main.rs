//! `golem-p2p`: try out golem-p2p between machines, and the per-machine test
//! client of the network test harness.
//!
//! ```text
//! golem-p2p keygen --out alice.key
//! golem-p2p directory --listen 0.0.0.0:8080
//! golem-p2p listen --key alice.key --directory http://dir:8080/ --relay https://relay.example/
//! golem-p2p send   --key bob.key   --directory http://dir:8080/ --relay https://relay.example/ \
//!                  --to 0x<alice node id> "hello"
//! golem-p2p --json probe --key bob.key --directory http://dir:8080/ \
//!                  --relay https://relay.example/ --to 0x<alice node id> --duration 30s --bulk 4M
//! ```
//!
//! Relay URLs must use `https`. For a relay with a self-signed certificate,
//! pass the certificate with `--relay-ca`. `--json` prints machine-readable
//! events, one JSON object per line, as described in `docs/cli-events.md`.

mod listen;
mod observe;
mod out;
mod parse;
mod probe;

use std::{
    net::SocketAddr,
    path::PathBuf,
    pin::pin,
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use golem_p2p::{
    Endpoint, HttpDirectory, Identities, InMemorySigner, MultiDiscovery, NodeId, RecordStore,
    TransportMode, iroh::tls::CaTlsConfig,
};
use rustls_pki_types::{CertificateDer, pem::PemObject};
use serde_json::json;
use tokio::sync::watch;

use crate::{
    listen::Limits,
    observe::{FirstDirect, Observer, strings},
    out::{Out, anyhow_kind, ms, path_name},
    parse::{QuicPort, RelaySpec},
    probe::{Bulk, Pinger},
};

#[derive(Parser)]
#[command(
    version,
    about = "Peer-to-peer messaging between Ethereum identities over iroh"
)]
struct Cli {
    #[command(flatten)]
    output: OutputArgs,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct OutputArgs {
    /// Print events as JSON lines on stdout (see docs/cli-events.md). Logs
    /// stay on stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Run ID, copied into every JSON event as `run`.
    #[arg(long, global = true, value_name = "ID")]
    run_id: Option<String>,
    /// Node name, copied into every JSON event as `node`.
    #[arg(long, global = true, value_name = "NAME")]
    node_name: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a secp256k1 identity key and print its node ID.
    Keygen {
        /// File to write the hex-encoded secret key to (mode 0600).
        #[arg(long)]
        out: PathBuf,
    },
    /// Run an HTTP directory of self-signed node records.
    Directory {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
    },
    /// Run a node that answers every request with an echo, and accepts
    /// uploads (`probe --bulk`) and serves downloads within limits.
    Listen {
        #[command(flatten)]
        node: NodeArgs,
        /// Exit after this long (default: run until SIGINT or SIGTERM).
        #[arg(long, value_parser = parse::duration)]
        duration: Option<Duration>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Send a request to a node and print the reply.
    Send {
        #[command(flatten)]
        node: NodeArgs,
        /// Node ID to send to.
        #[arg(long)]
        to: NodeId,
        /// Number of requests to send.
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// Message to send.
        message: String,
    },
    /// Measure reachability, network path and round-trip times to a node
    /// that runs `listen`, and optionally the throughput of a bulk transfer.
    ///
    /// Exit code: 0 if at least one request got a correct reply (and the bulk
    /// transfer, if requested, succeeded), 1 if no request succeeded or setup
    /// failed, 2 for invalid arguments, 3 if only the bulk transfer failed.
    Probe(ProbeArgs),
}

/// Server-side limits of `listen` for uploads and downloads.
#[derive(Args)]
struct LimitArgs {
    /// Largest upload or download per stream, e.g. `100M`. Larger uploads
    /// are stopped and larger downloads refused. Echo requests are limited to
    /// 1 MiB separately.
    #[arg(long, default_value = "100M", value_parser = parse::bytes)]
    max_transfer_size: u64,
    /// Uploads and downloads served at once, over all peers; more are
    /// refused as busy. 0 serves echo requests only.
    #[arg(long, default_value_t = 8)]
    max_transfers: usize,
    /// Longest time one upload or download may take.
    #[arg(long, default_value = "120s", value_parser = parse::duration)]
    max_transfer_time: Duration,
}

impl LimitArgs {
    fn limits(&self) -> Limits {
        Limits {
            max_transfer_size: self.max_transfer_size,
            max_transfers: self.max_transfers,
            max_transfer_time: self.max_transfer_time,
        }
    }
}

#[derive(Args)]
struct NodeArgs {
    /// Hex-encoded secret key file of the default identity.
    #[arg(long)]
    key: PathBuf,
    /// Additional identity key files (aliases).
    #[arg(long)]
    alias_key: Vec<PathBuf>,
    /// Relay server URL (https only). Repeat for several relays. Append
    /// `,quic=PORT` or `,quic=off` to set this relay's QUIC address discovery
    /// (QAD) port, e.g. `https://relay.example:56000,quic=56001`.
    #[arg(long, value_name = "URL[,quic=PORT|off]", value_parser = parse::relay_spec)]
    relay: Vec<RelaySpec>,
    /// QUIC address discovery (QAD) port of every relay without its own
    /// `,quic=` setting: a UDP port, or `off` to skip QAD. Default: iroh's
    /// default port 7842.
    #[arg(long, value_name = "PORT|off", value_parser = parse::quic_port)]
    relay_quic_port: Option<QuicPort>,
    /// PEM file with certificates to trust for relay TLS, in addition to the
    /// built-in web PKI roots. For relays with a self-signed certificate.
    #[arg(long)]
    relay_ca: Vec<PathBuf>,
    /// Directory URL. Repeat for several directories.
    #[arg(long, required = true)]
    directory: Vec<url::Url>,
    /// UDP address to bind (default: all interfaces, random port).
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Use relays only, no direct UDP.
    #[arg(long)]
    relay_only: bool,
    /// Disable UPnP/PCP/NAT-PMP port mapping.
    #[arg(long)]
    no_portmapper: bool,
}

impl NodeArgs {
    fn mode(&self) -> TransportMode {
        if self.relay_only {
            TransportMode::RelayOnly
        } else if self.relay.is_empty() {
            TransportMode::DirectOnly
        } else {
            TransportMode::DirectAndRelay
        }
    }
}

#[derive(Args)]
struct ProbeArgs {
    #[command(flatten)]
    node: NodeArgs,
    /// Node ID to probe.
    #[arg(long)]
    to: NodeId,
    /// How long to send requests: `30s`, `2m`, `500ms`; a plain number is
    /// seconds.
    #[arg(long, default_value = "30s", value_parser = parse::duration)]
    duration: Duration,
    /// Time between the starts of two requests.
    #[arg(long, default_value = "1s", value_parser = parse::duration)]
    interval: Duration,
    /// Request size in bytes (at most 1 MiB), e.g. `64`, `1k`, `512Ki`.
    #[arg(long, default_value = "64", value_parser = parse::bytes)]
    payload_size: u64,
    /// Timeout for connecting and for each request.
    #[arg(long, default_value = "10s", value_parser = parse::duration)]
    timeout: Duration,
    /// After the requests, send this many bytes to the listener in one stream
    /// and report the throughput, e.g. `4M` (decimal) or `4MiB`.
    #[arg(long, value_parser = parse::bytes)]
    bulk: Option<u64>,
    /// Timeout for the bulk transfer.
    #[arg(long, default_value = "60s", value_parser = parse::duration)]
    bulk_timeout: Duration,
}

fn read_certs(path: &PathBuf) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .with_context(|| format!("reading certificates from {}", path.display()))?;
    if certs.is_empty() {
        bail!("no PEM certificate found in {}", path.display());
    }
    Ok(certs)
}

fn read_key(path: &PathBuf) -> anyhow::Result<[u8; 32]> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut key = [0u8; 32];
    hex::decode_to_slice(text.trim().trim_start_matches("0x"), &mut key)
        .context("key file must contain 64 hex characters")?;
    Ok(key)
}

/// Binds an endpoint. `timeout` sets the connect and request timeouts.
async fn bind(args: &NodeArgs, timeout: Option<Duration>) -> anyhow::Result<Endpoint> {
    let signer = InMemorySigner::new();
    let default = signer.add_secret(&read_key(&args.key)?)?;
    let mut aliases = Vec::new();
    for k in &args.alias_key {
        aliases.push(signer.add_secret(&read_key(k)?)?);
    }
    let mut discovery = MultiDiscovery::new();
    for d in &args.directory {
        discovery = discovery.with(HttpDirectory::new(d.clone())?);
    }
    let mut b = Endpoint::builder(signer, Identities::with_aliases(default, aliases))
        .discovery(discovery)
        .transport_mode(args.mode())
        .portmapper(!args.no_portmapper);
    for r in &args.relay {
        b = b.relay_config(r.config(args.relay_quic_port));
    }
    if !args.relay_ca.is_empty() {
        let mut roots = Vec::new();
        for path in &args.relay_ca {
            roots.extend(read_certs(path)?);
        }
        b = b.relay_tls(CaTlsConfig::embedded().with_extra_roots(roots));
    }
    if let Some(addr) = args.bind {
        b = b.bind_addr(addr);
    }
    if let Some(t) = timeout {
        b = b.connect_timeout(t).request_timeout(t);
    }
    Ok(b.bind().await?)
}

/// A bound endpoint whose events are reported from the start.
struct Node {
    ep: Endpoint,
    _observer: Observer,
}

impl Node {
    /// Binds, emits the `ready` event and starts reporting endpoint events.
    async fn start(
        args: &NodeArgs,
        out: &Out,
        timeout: Option<Duration>,
        first_direct: Option<Arc<FirstDirect>>,
    ) -> anyhow::Result<Self> {
        let started = Instant::now();
        let ep = bind(args, timeout).await?;
        // Subscribe before `ready`, so nothing after it is missed.
        let events = ep.subscribe();
        let bind_time = started.elapsed();
        let rec = ep.local_record();
        let relay_config: Vec<_> = args
            .relay
            .iter()
            .map(|r| {
                let cfg = r.config(args.relay_quic_port);
                json!({"url": cfg.url.to_string(), "quic_port": cfg.quic.map(|q| q.port)})
            })
            .collect();
        out.event("ready")
            .field("node_id", ep.node_id().to_string())
            .field(
                "identities",
                strings(rec.identities().iter().map(|i| i.to_string())),
            )
            .field("endpoint_id", ep.endpoint_id().to_string())
            .field("instance_id", hex::encode(ep.instance_id()))
            .field("relays", strings(rec.record().relay_urls.iter().cloned()))
            .field(
                "direct_addrs",
                strings(rec.record().direct_addrs.iter().map(|a| a.to_string())),
            )
            .field("relay_config", relay_config)
            .field(
                "transport",
                match args.mode() {
                    TransportMode::DirectAndRelay => "direct_and_relay",
                    TransportMode::RelayOnly => "relay_only",
                    TransportMode::DirectOnly => "direct_only",
                },
            )
            .field("bind_ms", ms(bind_time))
            .emit();
        let observer = Observer::spawn(&ep, events, out, first_direct);
        Ok(Self {
            ep,
            _observer: observer,
        })
    }
}

/// Resolves on SIGINT or SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,golem_p2p=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let out = Out::new(cli.output.json, cli.output.run_id, cli.output.node_name);
    match run(cli.cmd, &out).await {
        Ok(code) => code,
        Err(e) => {
            out.event("error")
                .field("kind", anyhow_kind(&e, "fatal"))
                .field("message", format!("{e:#}"))
                .emit();
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cmd: Cmd, out: &Out) -> anyhow::Result<ExitCode> {
    match cmd {
        Cmd::Keygen { out: path } => {
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            let signer = InMemorySigner::new();
            let id = signer.generate();
            let secret = signer.secret(id).expect("generated");
            write_secret(&path, &hex::encode(secret))?;
            if out.json() {
                out.event("keygen")
                    .field("node_id", id.to_string())
                    .field("key_file", path.display().to_string())
                    .emit();
            } else {
                println!("{id}");
            }
        }
        Cmd::Directory { listen } => {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            let url = format!("http://{}/", listener.local_addr()?);
            if out.json() {
                out.event("directory").field("url", url).emit();
            } else {
                println!("directory listening on {url}");
            }
            let store = Arc::new(RecordStore::new(100_000));
            tokio::select! {
                r = golem_p2p::directory::serve(listener, store) => r?,
                _ = shutdown_signal() => {}
            }
        }
        Cmd::Listen {
            node,
            duration,
            limits,
        } => {
            let limits = limits.limits();
            let node = Node::start(&node, out, None, None).await?;
            let ep = &node.ep;
            if !out.json() {
                let rec = ep.local_record();
                println!(
                    "READY node_id={} identities={} endpoint_id={} relays={:?} addrs={:?}",
                    ep.node_id(),
                    rec.identities()
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    ep.endpoint_id(),
                    rec.record().relay_urls,
                    rec.record().direct_addrs,
                );
                eprintln!(
                    "limits: uploads and downloads of at most {} bytes, {} at once, {} s each; \
                     echo requests of at most {} bytes",
                    limits.max_transfer_size,
                    limits.max_transfers,
                    limits.max_transfer_time.as_secs_f64(),
                    listen::MAX_REQUEST
                );
            }
            let stop = async {
                match duration {
                    Some(d) => tokio::time::sleep(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = listen::serve(ep.clone(), out.clone(), limits) => {}
                _ = shutdown_signal() => {}
                _ = stop => {}
            }
            ep.close().await;
        }
        Cmd::Send {
            node,
            to,
            count,
            message,
        } => {
            let first_direct = Arc::new(FirstDirect::new(to));
            let node = Node::start(&node, out, None, Some(first_direct.clone())).await?;
            let mut pinger = Pinger::new(&node.ep, to, out, first_direct);
            let mut failure = None;
            for _ in 0..count {
                let r = pinger
                    .ping(message.as_bytes(), std::future::pending(), |_| true, true)
                    .await;
                match r.result {
                    Ok(reply) => {
                        if !out.json() {
                            println!(
                                "{} ({} ms, {})",
                                String::from_utf8_lossy(&reply),
                                r.rtt.as_millis(),
                                path_name(r.path)
                            );
                        }
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            pinger.summary(message.len(), false).emit(out, None);
            node.ep.close().await;
            if let Some(e) = failure {
                return Err(e);
            }
        }
        Cmd::Probe(args) => return run_probe(args, out).await,
    }
    Ok(ExitCode::SUCCESS)
}

async fn run_probe(args: ProbeArgs, out: &Out) -> anyhow::Result<ExitCode> {
    if args.interval.is_zero() {
        bail!("--interval must be positive");
    }
    let payload_size = usize::try_from(args.payload_size)
        .ok()
        .filter(|s| *s <= listen::MAX_REQUEST)
        .with_context(|| format!("--payload-size must be at most {}", listen::MAX_REQUEST))?;
    let first_direct = Arc::new(FirstDirect::new(args.to));
    let node = Node::start(
        &args.node,
        out,
        Some(args.timeout),
        Some(first_direct.clone()),
    )
    .await?;
    let ep = &node.ep;

    // SIGINT/SIGTERM end the probe early, with a summary.
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let stopped = || {
        let mut rx = stop_rx.clone();
        async move {
            let _ = rx.wait_for(|s| *s).await;
        }
    };

    let mut pinger = Pinger::new(ep, args.to, out, first_direct);
    let end = tokio::time::Instant::now() + args.duration;
    // A request in flight at the end may take up to one more timeout.
    let hard_end = end + args.timeout;
    let mut tick = tokio::time::interval(args.interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut interrupted = false;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stopped() => {
                interrupted = true;
                break;
            }
        }
        if tokio::time::Instant::now() >= end {
            break;
        }
        let payload = probe::payload(pinger.sent() + 1, payload_size);
        let cancel = async {
            tokio::select! {
                _ = tokio::time::sleep_until(hard_end) => {
                    anyhow::Error::from(golem_p2p::Error::Timeout)
                        .context("probe ended before the reply")
                }
                _ = stopped() => anyhow!("interrupted"),
            }
        };
        let r = pinger
            .ping(&payload, cancel, |reply| reply.ends_with(&payload), false)
            .await;
        if !out.json() {
            match &r.result {
                Ok(_) => println!(
                    "seq={} bytes={} rtt_ms={} path={}",
                    r.seq,
                    payload.len(),
                    ms(r.rtt),
                    path_name(r.path)
                ),
                Err(e) => println!("seq={} failed after {} ms: {e:#}", r.seq, ms(r.rtt)),
            }
        }
        if *stop_rx.borrow() {
            interrupted = true;
            break;
        }
    }

    let bulk = match args.bulk {
        Some(bytes) if !interrupted && pinger.ok() > 0 => {
            let mut stop = pin!(stopped());
            Some(tokio::select! {
                b = Bulk::run(ep, args.to, bytes, args.bulk_timeout) => b,
                _ = &mut stop => Bulk { bytes, result: Err(anyhow!("interrupted")) },
            })
        }
        Some(bytes) => Some(Bulk {
            bytes,
            result: Err(anyhow!(if interrupted {
                "interrupted"
            } else {
                "skipped: no successful reply"
            })),
        }),
        None => None,
    };
    if let Some(b) = &bulk {
        b.report_error(out);
    }
    let summary = pinger.summary(payload_size, interrupted);
    if out.json() {
        summary.emit(out, bulk.as_ref());
    } else {
        summary.print_human(bulk.as_ref());
    }
    ep.close().await;
    Ok(if pinger.ok() == 0 {
        ExitCode::from(1)
    } else if bulk.is_some_and(|b| b.result.is_err()) {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}

fn write_secret(path: &PathBuf, contents: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}
