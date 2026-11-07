#!/usr/bin/env python3
"""UDP/TCP address reflector for the golem-p2p NAT lab.

Serves on several UDP sockets (different IPs and ports) and optional TCP
listeners. Text protocol, one datagram per message:

  MAP <nonce>
      Reply from the same socket: "MAPPED <nonce> <observed ip:port> VIA <local ip:port>".
      Used to measure the mapping (external address) a NAT allocates.

  FILT <nonce> [<port> ...]
      Same reply as MAP, then, after a short delay, unsolicited packets
      "UNSOL <nonce> FROM <local ip:port>" to the observed address from every
      *other* UDP socket of this reflector (other port, other IP, both). For
      each extra <port>, one more "UNSOL ... TOPORT <port>" from the receiving
      socket to <observed ip>:<port>, a port the client never sent from.
      Used to measure the NAT's filtering behaviour.

TCP: on accept, writes "MAPPED tcp <observed ip:port> VIA <local ip:port>\\n"
and closes.

The reflector only ever answers well-formed messages, with replies no larger
than the request plus a few dozen bytes, to the observed source address.
"""

import argparse
import heapq
import selectors
import socket
import sys
import time


def parse_addr(s):
    host, port = s.rsplit(":", 1)
    return host, int(port)


def fmt(addr):
    return "%s:%d" % (addr[0], addr[1])


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--udp", action="append", default=[], help="ip:port to serve on (repeat)")
    ap.add_argument("--tcp", action="append", default=[], help="ip:port to accept TCP on (repeat)")
    ap.add_argument("--delay", type=float, default=0.2, help="seconds before unsolicited packets")
    ap.add_argument("--max-extra-ports", type=int, default=4)
    args = ap.parse_args()
    if not args.udp and not args.tcp:
        ap.error("nothing to serve")

    sel = selectors.DefaultSelector()
    udp = []
    for spec in args.udp:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(parse_addr(spec))
        s.setblocking(False)
        sel.register(s, selectors.EVENT_READ, "udp")
        udp.append(s)
    for spec in args.tcp:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(parse_addr(spec))
        s.listen(64)
        s.setblocking(False)
        sel.register(s, selectors.EVENT_READ, "tcp")
    print("reflector ready udp=%s tcp=%s" % (",".join(args.udp), ",".join(args.tcp)), flush=True)

    pending = []  # heap of (when, seq, sock, payload, dest)
    seq = 0
    while True:
        timeout = None
        if pending:
            timeout = max(0.0, pending[0][0] - time.monotonic())
        for key, _ in sel.select(timeout):
            sock = key.fileobj
            if key.data == "tcp":
                try:
                    conn, src = sock.accept()
                except OSError:
                    continue
                try:
                    conn.settimeout(1.0)
                    conn.sendall(("MAPPED tcp %s VIA %s\n" % (fmt(src), fmt(conn.getsockname()))).encode())
                except OSError:
                    pass
                finally:
                    conn.close()
                continue
            try:
                data, src = sock.recvfrom(2048)
            except OSError:
                continue
            parts = data.decode("ascii", errors="replace").split()
            if len(parts) < 2 or parts[0] not in ("MAP", "FILT") or len(parts[1]) > 32:
                continue
            nonce = parts[1]
            local = fmt(sock.getsockname())
            try:
                sock.sendto(("MAPPED %s %s VIA %s" % (nonce, fmt(src), local)).encode(), src)
            except OSError:
                pass
            if parts[0] != "FILT":
                continue
            when = time.monotonic() + args.delay
            for other in udp:
                if other is sock:
                    continue
                msg = "UNSOL %s FROM %s" % (nonce, fmt(other.getsockname()))
                seq += 1
                heapq.heappush(pending, (when, seq, other, msg.encode(), src))
            for p in parts[2 : 2 + args.max_extra_ports]:
                try:
                    port = int(p)
                except ValueError:
                    continue
                if not 1 <= port <= 65535:
                    continue
                msg = "UNSOL %s FROM %s TOPORT %d" % (nonce, local, port)
                seq += 1
                heapq.heappush(pending, (when, seq, sock, msg.encode(), (src[0], port)))
        now = time.monotonic()
        while pending and pending[0][0] <= now:
            _, _, s, payload, dest = heapq.heappop(pending)
            try:
                s.sendto(payload, dest)
            except OSError:
                pass


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
