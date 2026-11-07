//! `golem-p2p`: try out golem-p2p between two machines.
//!
//! ```text
//! golem-p2p keygen --out alice.key
//! golem-p2p directory --listen 0.0.0.0:8080
//! golem-p2p listen --key alice.key --directory http://dir:8080/ --relay https://relay.example/
//! golem-p2p send   --key bob.key   --directory http://dir:8080/ --relay https://relay.example/ \
//!                  --to 0x<alice node id> "hello"
//! ```

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use golem_p2p::{
    Endpoint, HttpDirectory, Identities, InMemorySigner, MultiDiscovery, NodeId, PathKind,
    RecordStore, TransportMode, iroh::RelayUrl,
};

#[derive(Parser)]
#[command(
    version,
    about = "Peer-to-peer messaging between Ethereum identities over iroh"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
    /// Run a node that answers every request with an echo.
    Listen(NodeArgs),
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
}

#[derive(Args)]
struct NodeArgs {
    /// Hex-encoded secret key file of the default identity.
    #[arg(long)]
    key: PathBuf,
    /// Additional identity key files (aliases).
    #[arg(long)]
    alias_key: Vec<PathBuf>,
    /// Relay server URL. Repeat for several relays.
    #[arg(long)]
    relay: Vec<RelayUrl>,
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

fn read_key(path: &PathBuf) -> anyhow::Result<[u8; 32]> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut key = [0u8; 32];
    hex::decode_to_slice(text.trim().trim_start_matches("0x"), &mut key)
        .context("key file must contain 64 hex characters")?;
    Ok(key)
}

async fn bind(args: &NodeArgs) -> anyhow::Result<Endpoint> {
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
    let mode = if args.relay_only {
        TransportMode::RelayOnly
    } else if args.relay.is_empty() {
        TransportMode::DirectOnly
    } else {
        TransportMode::DirectAndRelay
    };
    let mut b = Endpoint::builder(signer, Identities::with_aliases(default, aliases))
        .discovery(discovery)
        .transport_mode(mode)
        .portmapper(!args.no_portmapper);
    for r in &args.relay {
        b = b.relay(r.clone());
    }
    if let Some(addr) = args.bind {
        b = b.bind_addr(addr);
    }
    Ok(b.bind().await?)
}

fn path_name(p: PathKind) -> &'static str {
    match p {
        PathKind::Direct => "direct",
        PathKind::Relay => "relay",
        PathKind::Unknown => "unknown",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,golem_p2p=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen { out } => {
            if out.exists() {
                bail!("{} already exists", out.display());
            }
            let signer = InMemorySigner::new();
            let id = signer.generate();
            let secret = signer.secret(id).expect("generated");
            write_secret(&out, &hex::encode(secret))?;
            println!("{id}");
        }
        Cmd::Directory { listen } => {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            println!("directory listening on http://{}/", listener.local_addr()?);
            let store = Arc::new(RecordStore::new(100_000));
            tokio::select! {
                r = golem_p2p::directory::serve(listener, store) => r?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Cmd::Listen(args) => {
            let ep = bind(&args).await?;
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
            let endpoint_id = ep.endpoint_id().to_string();
            let instance = hex::encode(ep.instance_id());
            let server = ep.clone();
            let serve = async move {
                while let Some(mut inc) = server.accept().await {
                    let endpoint_id = endpoint_id.clone();
                    let instance = instance.clone();
                    tokio::spawn(async move {
                        let Ok(req) = inc.read_request(1 << 20).await else {
                            return;
                        };
                        let path = path_name(inc.connection().path());
                        eprintln!(
                            "request from {} to {} via {path}: {}",
                            inc.from(),
                            inc.to(),
                            String::from_utf8_lossy(&req)
                        );
                        let reply = format!(
                            "endpoint={endpoint_id} instance={instance} to={} from={} echo={}",
                            inc.to(),
                            inc.from(),
                            String::from_utf8_lossy(&req)
                        );
                        let _ = inc.respond(reply.as_bytes()).await;
                    });
                }
            };
            tokio::select! {
                _ = serve => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            ep.close().await;
        }
        Cmd::Send {
            node,
            to,
            count,
            message,
        } => {
            let ep = bind(&node).await?;
            for _ in 0..count {
                let start = Instant::now();
                let reply = ep.request(to, message.as_bytes()).await?;
                let path = path_name(ep.connect(to).await?.path());
                println!(
                    "{} ({} ms, {path})",
                    String::from_utf8_lossy(&reply),
                    start.elapsed().as_millis()
                );
            }
            ep.close().await;
        }
    }
    Ok(())
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
