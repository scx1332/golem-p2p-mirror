//! Which of the endpoint's direct addresses go into its record.
//!
//! iroh reports every address the endpoint might be reached on: the public
//! address the relays saw (QUIC address discovery, QAD) and the addresses of
//! all local interfaces that are up. On a host with Docker, libvirt or
//! Kubernetes that includes the bridges to its own containers and VMs
//! (`docker0` with 172.17.0.1, Docker's `br-<id>` networks, `virbr0`, ...).
//! Those networks end at this host: on a peer, 172.17.0.1 is the peer's own
//! Docker bridge. Publishing them only costs peers dial attempts (TLS makes
//! them harmless), and since a record holds at most [`MAX_DIRECT_ADDRS`]
//! addresses, enough of them would push out the addresses that matter.
//!
//! So before signing, the candidates are
//!
//! 1. **filtered:** an address is left out if every local interface that
//!    carries it is host-internal ([`is_host_internal`], matched by interface
//!    name; address ranges alone cannot tell a Docker bridge from a real
//!    172.16/12 or 10/8 LAN) or matches a pattern of
//!    [`Builder::exclude_interface`](crate::Builder::exclude_interface);
//! 2. **ranked:** by address scope (global, then private, then loopback) and
//!    within a scope addresses that are on no local interface (learned from
//!    relays by QAD) before interface addresses; within each of these groups
//!    IPv4 and IPv6 alternate, each sorted by address;
//! 3. **capped** at [`MAX_DIRECT_ADDRS`], so the cap drops the least useful
//!    addresses, never a public one while a private one stays.
//!
//! VPN interfaces (`wg*`, `tailscale0`, `zt*`, `tun*`, ...) are not
//! host-internal: their networks span machines, and two peers on the same VPN
//! may use them. Their addresses are private, CGNAT or ULA in practice, so
//! they rank below public ones and can never displace them.
//!
//! This is publishing policy only. Verification accepts every record within
//! the bounds, also one that lists Docker addresses (older nodes).

use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use crate::record::MAX_DIRECT_ADDRS;

/// Interfaces whose network ends at this host: bridges and virtual links to
/// its own containers and VMs. A trailing `*` matches any suffix. Docker's
/// user-defined networks (`br-` and 12 hex digits) are matched separately.
const HOST_INTERNAL_INTERFACES: &[&str] = &[
    "docker*",         // Docker: docker0, docker_gwbridge
    "veth*",           // host side of container veth pairs
    "virbr*",          // libvirt NAT and isolated networks
    "lxc*",            // LXC (lxcbr0), Cilium endpoints
    "lxdbr*",          // LXD
    "incusbr*",        // Incus
    "podman*",         // Podman (netavark)
    "nerdctl*",        // containerd / nerdctl
    "cni*",            // CNI bridge plugin: cni0, cni-podman0
    "flannel*",        // Kubernetes flannel: flannel.1
    "cali*",           // Kubernetes Calico
    "vxlan.calico",    // Calico VXLAN
    "vxlan-v6.calico", // Calico VXLAN, IPv6
    "cilium_*",        // Kubernetes Cilium
    "kube-*",          // kube-proxy IPVS (kube-ipvs0), kube-router (kube-bridge)
    "weave*",          // Weave Net
    "vmnet*",          // VMware host-only and NAT networks
    "vboxnet*",        // VirtualBox host-only networks
];

/// `pattern` is an interface name, or a prefix followed by `*`.
fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

/// Docker names the bridge of a user-defined network `br-` plus the first 12
/// hex digits of the network ID. Other `br-` names (OpenWrt's `br-lan`, an
/// admin's own bridges) often carry the host's real LAN address.
fn is_docker_network(name: &str) -> bool {
    name.strip_prefix("br-").is_some_and(|id| {
        id.len() == 12 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Returns `true` if the interface named `name` only connects this host to
/// its own containers or VMs, so that its addresses are useless to peers.
pub(crate) fn is_host_internal(name: &str) -> bool {
    is_docker_network(name) || HOST_INTERNAL_INTERFACES.iter().any(|p| matches(p, name))
}

/// Which local interfaces carry which IP address.
#[derive(Debug, Clone, Default)]
pub(crate) struct Interfaces(BTreeMap<IpAddr, Vec<String>>);

impl Interfaces {
    /// Enumerates the machine's interfaces, with the same crate iroh uses to
    /// find its local addresses (`netwatch`), so both see the same interfaces.
    pub(crate) async fn current() -> Self {
        let state = netwatch::interfaces::State::new().await;
        Self::from_pairs(state.interfaces.values().flat_map(|iface| {
            iface
                .addrs()
                .map(move |net| (iface.name().to_string(), net.addr()))
        }))
    }

    /// Builds the map from `(interface name, address)` pairs.
    pub(crate) fn from_pairs(pairs: impl IntoIterator<Item = (String, IpAddr)>) -> Self {
        let mut map = BTreeMap::<IpAddr, Vec<String>>::new();
        for (name, ip) in pairs {
            let names = map.entry(ip.to_canonical()).or_default();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        for names in map.values_mut() {
            names.sort();
        }
        Self(map)
    }

    fn names(&self, ip: IpAddr) -> &[String] {
        self.0.get(&ip.to_canonical()).map_or(&[], Vec::as_slice)
    }
}

/// Reachability class of an address, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    Global,
    /// RFC 1918, CGNAT (100.64/10), link-local, IPv6 ULA and the rest.
    Private,
    Loopback,
}

fn scope(ip: IpAddr) -> Scope {
    match ip.to_canonical() {
        IpAddr::V4(ip) if ip.is_loopback() => Scope::Loopback,
        IpAddr::V4(ip) if is_private_v4(ip) => Scope::Private,
        IpAddr::V4(_) => Scope::Global,
        IpAddr::V6(ip) if ip.is_loopback() => Scope::Loopback,
        // Global unicast is 2000::/3; ULA (fc00::/7), link-local and the
        // special ranges are not reachable from the internet.
        IpAddr::V6(ip) if is_global_unicast_v6(ip) => Scope::Global,
        IpAddr::V6(_) => Scope::Private,
    }
}

fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        // Shared address space (RFC 6598): carrier-grade NAT, Tailscale.
        || (a == 100 && (64..128).contains(&b))
}

fn is_global_unicast_v6(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xe000 == 0x2000
}

/// Where an address came from, as far as we can tell, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    /// On no local interface: a relay saw it (QAD), i.e. the address the
    /// internet sees after any NAT.
    Observed,
    /// The address of a local interface.
    Interface,
}

/// The result of [`select`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    /// The addresses to publish, best first, at most [`MAX_DIRECT_ADDRS`].
    pub published: Vec<SocketAddr>,
    /// Left out because they are only on host-internal or excluded
    /// interfaces, with the (first) interface name.
    pub excluded: Vec<(SocketAddr, String)>,
    /// Left out by the cap, best first.
    pub over_cap: Vec<SocketAddr>,
}

/// Filters, ranks and caps the candidate direct addresses (see the module
/// documentation). `exclude` holds additional interface patterns. The result
/// does not depend on the order of `candidates`.
pub(crate) fn select(
    candidates: impl IntoIterator<Item = SocketAddr>,
    interfaces: &Interfaces,
    exclude: &[String],
) -> Selection {
    let excluded_iface =
        |name: &str| is_host_internal(name) || exclude.iter().any(|p| matches(p, name));
    let mut excluded = Vec::new();
    // Per (scope, origin): IPv4 and IPv6 addresses, each sorted.
    let mut groups = BTreeMap::<(Scope, Origin), (Vec<SocketAddr>, Vec<SocketAddr>)>::new();
    for addr in candidates.into_iter().collect::<BTreeSet<_>>() {
        let names = interfaces.names(addr.ip());
        let origin = if names.is_empty() {
            Origin::Observed
        } else if names.iter().all(|n| excluded_iface(n)) {
            excluded.push((addr, names[0].clone()));
            continue;
        } else {
            Origin::Interface
        };
        let (v4, v6) = groups.entry((scope(addr.ip()), origin)).or_default();
        if addr.ip().to_canonical().is_ipv4() {
            v4.push(addr);
        } else {
            v6.push(addr);
        }
    }
    let mut ranked = Vec::new();
    for (v4, v6) in groups.into_values() {
        let (mut v4, mut v6) = (v4.into_iter(), v6.into_iter());
        loop {
            let (a, b) = (v4.next(), v6.next());
            if a.is_none() && b.is_none() {
                break;
            }
            ranked.extend(a);
            ranked.extend(b);
        }
    }
    let over_cap = ranked.split_off(ranked.len().min(MAX_DIRECT_ADDRS));
    Selection {
        published: ranked,
        excluded,
        over_cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn ifaces(pairs: &[(&str, &str)]) -> Interfaces {
        Interfaces::from_pairs(
            pairs
                .iter()
                .map(|(n, ip)| (n.to_string(), ip.parse().unwrap())),
        )
    }

    #[test]
    fn host_internal_interfaces() {
        for name in [
            "docker0",
            "docker_gwbridge",
            "br-2a56f21d48f6",
            "veth3447eb9",
            "virbr0",
            "virbr0-nic",
            "lxcbr0",
            "lxdbr0",
            "incusbr0",
            "podman0",
            "cni0",
            "cni-podman0",
            "flannel.1",
            "cali1234abcd",
            "vxlan.calico",
            "cilium_host",
            "kube-ipvs0",
            "kube-bridge",
            "weave",
            "vmnet8",
            "vboxnet0",
            "nerdctl0",
        ] {
            assert!(is_host_internal(name), "{name}");
        }
        for name in [
            "eth0",
            "enp4s0",
            "wlan0",
            "wlp2s0",
            "en0",
            "lo",
            // Admin bridges and Proxmox carry the host's real LAN address.
            "br0",
            "br-lan",
            "br-2A56F21D48F6",
            "br-2a56f21d48f",
            "bridge0",
            "vmbr0",
            "bond0",
            "eth0.100",
            "ppp0",
            // VPNs connect machines: kept by default.
            "wg0",
            "tailscale0",
            "zt5u4y6bwn",
            "tun0",
            "utun3",
        ] {
            assert!(!is_host_internal(name), "{name}");
        }
    }

    #[test]
    fn scopes() {
        for ip in ["65.109.154.126", "2a01:4f9:3080:1146::2", "203.0.113.7"] {
            assert_eq!(scope(ip.parse().unwrap()), Scope::Global, "{ip}");
        }
        for ip in [
            "10.1.2.3",
            "172.17.0.1",
            "192.168.1.10",
            "100.101.102.103",
            "169.254.1.1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
        ] {
            assert_eq!(scope(ip.parse().unwrap()), Scope::Private, "{ip}");
        }
        for ip in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            assert_eq!(scope(ip.parse().unwrap()), Scope::Loopback, "{ip}");
        }
        assert_eq!(scope("100.63.0.1".parse().unwrap()), Scope::Global);
        assert_eq!(scope("100.128.0.1".parse().unwrap()), Scope::Global);
    }

    /// This host: a public IPv4 and IPv6 on the uplink, Docker bridges.
    #[test]
    fn docker_host() {
        let mut pairs: Vec<(String, IpAddr)> = [
            ("lo", "127.0.0.1"),
            ("enp4s0", "65.109.154.126"),
            ("enp4s0", "2a01:4f9:3080:1146::2"),
            ("enp4s0", "fe80::aaa1:59ff:fec2:88f4"),
            ("docker0", "172.17.0.1"),
        ]
        .iter()
        .map(|(n, ip)| (n.to_string(), ip.parse().unwrap()))
        .collect();
        let ids = [
            "2a56f21d48f6",
            "3d4496a3ac9c",
            "a8a62937f3b7",
            "310b21aa873d",
        ];
        for (i, id) in ids.iter().enumerate() {
            let ip = format!("172.{}.0.1", 18 + i).parse().unwrap();
            pairs.push((format!("br-{id}"), ip));
        }
        let ifs = Interfaces::from_pairs(pairs);
        let mut cands = vec![
            sa("65.109.154.126:39009"),
            sa("[2a01:4f9:3080:1146::2]:42374"),
            sa("172.17.0.1:39009"),
        ];
        cands.extend((18..22).map(|b| sa(&format!("172.{b}.0.1:39009"))));
        let s = select(cands, &ifs, &[]);
        assert_eq!(
            s.published,
            [
                sa("65.109.154.126:39009"),
                sa("[2a01:4f9:3080:1146::2]:42374")
            ]
        );
        assert_eq!(s.excluded.len(), 5);
        assert_eq!(s.excluded[0], (sa("172.17.0.1:39009"), "docker0".into()));
        assert_eq!(s.excluded[1].1, "br-2a56f21d48f6");
        assert!(s.over_cap.is_empty());
    }

    /// More than 16 candidates, the QAD address and the public IPv6 sorting
    /// last: the cap drops private LAN addresses, never those.
    #[test]
    fn cap_keeps_the_best() {
        // 20 real LAN addresses (not host-internal, so not filtered).
        let mut pairs: Vec<(String, IpAddr)> = (0..20)
            .map(|i| (format!("eth{i}"), format!("10.0.{i}.2").parse().unwrap()))
            .collect();
        pairs.push(("eth0".into(), "2001:db8::5".parse().unwrap()));
        pairs.push(("eth0".into(), "fd00::5".parse().unwrap()));
        let ifs = Interfaces::from_pairs(pairs);
        let mut cands: Vec<SocketAddr> = (0..20).map(|i| sa(&format!("10.0.{i}.2:4000"))).collect();
        // The address a relay saw (behind NAT: on no local interface).
        cands.push(sa("203.0.113.7:61000"));
        cands.push(sa("[2001:db8::5]:4001"));
        cands.push(sa("[fd00::5]:4001"));
        // Lexicographically the public ones come last.
        let mut sorted = cands.clone();
        sorted.sort();
        assert!(!sorted[..MAX_DIRECT_ADDRS].contains(&sa("203.0.113.7:61000")));
        assert!(!sorted[..MAX_DIRECT_ADDRS].contains(&sa("[2001:db8::5]:4001")));

        let s = select(cands.clone(), &ifs, &[]);
        assert_eq!(s.published.len(), MAX_DIRECT_ADDRS);
        assert_eq!(s.published[0], sa("203.0.113.7:61000"));
        assert_eq!(s.published[1], sa("[2001:db8::5]:4001"));
        // Private: IPv4 and IPv6 alternate, so the ULA is not starved.
        assert_eq!(s.published[2], sa("10.0.0.2:4000"));
        assert_eq!(s.published[3], sa("[fd00::5]:4001"));
        assert_eq!(s.published[4], sa("10.0.1.2:4000"));
        assert_eq!(s.over_cap.len(), 23 - MAX_DIRECT_ADDRS);
        assert!(s.over_cap.iter().all(|a| scope(a.ip()) == Scope::Private));
        assert!(s.excluded.is_empty());

        // Deterministic: the input order does not matter.
        cands.reverse();
        assert_eq!(select(cands, &ifs, &[]), s);
    }

    #[test]
    fn ranking_and_lan() {
        let ifs = ifaces(&[
            ("lo", "127.0.0.1"),
            ("eth0", "192.168.1.10"),
            ("eth0", "2001:db8:1::10"),
            ("eth0", "2001:db8:1::11"),
            ("br0", "10.0.0.2"),
            ("tailscale0", "100.101.102.103"),
            ("tailscale0", "fd7a:115c:a1e0::1"),
            ("wg0", "10.8.0.2"),
            ("virbr0", "192.168.122.1"),
        ]);
        let cands = [
            "127.0.0.1:5",
            "192.168.1.10:5",
            "[2001:db8:1::10]:6",
            "[2001:db8:1::11]:6",
            "10.0.0.2:5",
            "100.101.102.103:5",
            "[fd7a:115c:a1e0::1]:6",
            "10.8.0.2:5",
            "192.168.122.1:5",
            "198.51.100.1:7",
        ]
        .map(sa);
        let s = select(cands, &ifs, &[]);
        assert_eq!(
            s.published,
            [
                // Observed (QAD), then interface addresses, global first.
                "198.51.100.1:7",
                "[2001:db8:1::10]:6",
                "[2001:db8:1::11]:6",
                // Private LAN and VPN addresses are kept, after public ones.
                "10.0.0.2:5",
                "[fd7a:115c:a1e0::1]:6",
                "10.8.0.2:5",
                "100.101.102.103:5",
                "192.168.1.10:5",
                "127.0.0.1:5",
            ]
            .map(sa)
        );
        assert_eq!(s.excluded, [(sa("192.168.122.1:5"), "virbr0".into())]);

        // Extra patterns: exact names and prefixes.
        let extra = ["wg*".to_string(), "tailscale0".to_string()];
        let s = select(cands, &ifs, &extra);
        let names: Vec<_> = s.excluded.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["wg0", "tailscale0", "virbr0", "tailscale0"]);
        assert_eq!(s.published.len(), 6);
    }

    /// An address on a host-internal and a normal interface is kept.
    #[test]
    fn shared_address_kept() {
        let ifs = ifaces(&[
            ("kube-ipvs0", "65.21.0.9"),
            ("eth0", "65.21.0.9"),
            ("kube-ipvs0", "10.96.0.1"),
        ]);
        let s = select([sa("65.21.0.9:1"), sa("10.96.0.1:1")], &ifs, &[]);
        assert_eq!(s.published, [sa("65.21.0.9:1")]);
        assert_eq!(s.excluded, [(sa("10.96.0.1:1"), "kube-ipvs0".into())]);
    }

    /// The real enumeration finds this machine's loopback interface.
    #[tokio::test]
    async fn current_interfaces() {
        let ifs = Interfaces::current().await;
        let lo = ifs.names(Ipv4Addr::LOCALHOST.into());
        assert!(!lo.is_empty(), "{ifs:?}");
        assert!(lo.iter().all(|n| !is_host_internal(n)), "{lo:?}");
    }

    #[test]
    fn no_candidates() {
        assert_eq!(
            select([], &Interfaces::default(), &[]),
            Selection::default()
        );
    }
}
