#!/usr/bin/env python3
"""Classify the NAT in front of this host, using reflector.py (RFC 4787 terms).

Mapping test: one fresh UDP socket sends MAP to every --map target and
compares the external address each one reports:
  no-nat  the reflectors see our own address and port
  EIM     the same external ip:port toward every destination
  ADM     the same toward one destination IP, different toward another IP
  APDM    different even toward another port on the same IP ("symmetric")
  no-udp  no reply at all

Filtering test: another fresh socket sends FILT to --filt only. The reflector
then sends unsolicited packets from its other sockets (other port on the same
IP, other IP, other IP and port). Which of them arrive:
  EIF     a packet from another IP got in ("full cone")
  ADF     only a packet from the same IP (another port) got in ("restricted cone")
  APDF    none got in ("port-restricted cone"; also what symmetric NATs do)
A third socket, bound but never used, checks whether packets to an unmapped
external port get in at all (only expected without NAT or firewall).

TCP test: connects to --tcp and reads the observed address.

Prints one line of key=value pairs, or JSON with --json.
"""

import argparse
import json
import os
import socket
import sys
import time


def parse_addr(s):
    host, port = s.rsplit(":", 1)
    return host, int(port)


def query(sock, dest, verb, extra, timeout, tries):
    """Sends `verb nonce extra...` until a MAPPED reply arrives. Returns observed ip:port or None."""
    nonce = os.urandom(6).hex()
    msg = " ".join([verb, nonce] + [str(e) for e in extra]).encode()
    for _ in range(tries):
        sock.sendto(msg, dest)
        deadline = time.monotonic() + timeout
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                break
            sock.settimeout(left)
            try:
                data, _ = sock.recvfrom(2048)
            except (socket.timeout, ConnectionRefusedError, OSError):
                continue
            parts = data.decode("ascii", errors="replace").split()
            if len(parts) >= 3 and parts[0] == "MAPPED" and parts[1] == nonce:
                return parts[2], nonce
    return None, nonce


def local_ip_toward(dest):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(dest)
        return s.getsockname()[0]
    finally:
        s.close()


def mapping_test(targets, timeout, tries):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 0))
    lport = s.getsockname()[1]
    lip = local_ip_toward(targets[0])
    observed = []
    for t in targets:
        obs, _ = query(s, t, "MAP", [], timeout, tries)
        observed.append(obs)
    s.close()
    got = [o for o in observed if o]
    if not got:
        kind = "no-udp"
    elif len(got) < len(observed):
        kind = "partial"
    elif all(o == "%s:%d" % (lip, lport) for o in got):
        kind = "no-nat"
    elif len(set(got)) == 1:
        kind = "EIM"
    else:
        # Same destination IP gives the same mapping, but another IP a different one?
        by_ip = {}
        for t, o in zip(targets, observed):
            by_ip.setdefault(t[0], set()).add(o)
        if all(len(v) == 1 for v in by_ip.values()):
            kind = "ADM"
        else:
            kind = "APDM"
    preserved = bool(got) and all(parse_addr(o)[1] == lport for o in got)
    return {
        "mapping": kind,
        "local": "%s:%d" % (lip, lport),
        "mapped": [o or "-" for o in observed],
        "port_preserved": preserved,
    }


def filtering_test(primary, timeout, tries, wait):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 0))
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    probe.bind(("0.0.0.0", 0))  # never sends: checks inbound to an unmapped port
    pport = probe.getsockname()[1]
    obs, nonce = query(s, primary, "FILT", [pport], timeout, tries)
    if obs is None:
        s.close()
        probe.close()
        return {"filtering": "n/a", "unsolicited": [], "unmapped_inbound": False}
    got = set()
    unmapped = False
    deadline = time.monotonic() + wait
    while True:
        left = deadline - time.monotonic()
        if left <= 0:
            break
        for sock in (s, probe):
            sock.settimeout(min(left, 0.05))
            try:
                data, src = sock.recvfrom(2048)
            except (socket.timeout, ConnectionRefusedError, OSError):
                continue
            parts = data.decode("ascii", errors="replace").split()
            if len(parts) >= 4 and parts[0] == "UNSOL" and parts[1] == nonce:
                if sock is probe:
                    unmapped = True
                else:
                    got.add("%s:%d" % src)
    s.close()
    probe.close()
    pip, pport_ = primary
    other_ip = any(parse_addr(g)[0] != pip for g in got)
    same_ip_other_port = any(parse_addr(g)[0] == pip and parse_addr(g)[1] != pport_ for g in got)
    if other_ip:
        kind = "EIF"
    elif same_ip_other_port:
        kind = "ADF"
    else:
        kind = "APDF"
    return {"filtering": kind, "unsolicited": sorted(got), "unmapped_inbound": unmapped}


def tcp_test(dest, timeout):
    try:
        with socket.create_connection(dest, timeout=timeout) as c:
            c.settimeout(timeout)
            data = c.recv(256).decode("ascii", errors="replace").split()
            if len(data) >= 3 and data[0] == "MAPPED":
                return {"tcp": True, "tcp_mapped": data[2]}
    except OSError:
        pass
    return {"tcp": False, "tcp_mapped": "-"}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--map", action="append", default=[], help="reflector ip:port for the mapping test (repeat; give two ports on one IP and one other IP)")
    ap.add_argument("--filt", help="reflector ip:port for the filtering test")
    ap.add_argument("--tcp", help="reflector ip:port for the TCP test")
    ap.add_argument("--timeout", type=float, default=0.7, help="seconds to wait per try")
    ap.add_argument("--tries", type=int, default=3)
    ap.add_argument("--wait", type=float, default=1.0, help="seconds to collect unsolicited packets")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    res = {}
    if args.map:
        res.update(mapping_test([parse_addr(m) for m in args.map], args.timeout, args.tries))
    if args.filt:
        res.update(filtering_test(parse_addr(args.filt), args.timeout, args.tries, args.wait))
    if args.tcp:
        res.update(tcp_test(parse_addr(args.tcp), args.timeout * args.tries))
    if args.json:
        print(json.dumps(res, sort_keys=True))
    else:
        out = []
        for k in sorted(res):
            v = res[k]
            if isinstance(v, list):
                v = ",".join(v) or "-"
            elif isinstance(v, bool):
                v = "yes" if v else "no"
            out.append("%s=%s" % (k, v))
        print(" ".join(out))


if __name__ == "__main__":
    sys.exit(main())
