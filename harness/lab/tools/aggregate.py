#!/usr/bin/env python3
"""Aggregates `lab.sh matrix` / realworld.sh results.txt into a Markdown table.

  aggregate.py results.txt [--topology topology.topo]

One row per listener/sender pair, over all runs:
  connected       runs with at least one reply / runs
  final path      path after the last request, per run (d = direct, r = relay)
  t_direct        seconds from sender start to the first direct reply (median over
                  runs that went direct; "-" if none did)
  first request   median ms of the first request (lookup + connect + 1 request)
  direct %        median share of replies on a direct path
  RTT d/r         median request ms on direct / relay paths
"""

import argparse
import re
import shlex
import statistics
import sys


def parse_line(line):
    out = {}
    for tok in shlex.split(line):
        if "=" in tok:
            k, v = tok.split("=", 1)
            out[k] = v
    return out


def med(xs, fmt="%g"):
    xs = [float(x) for x in xs if x not in ("-", "", None)]
    if not xs:
        return "-"
    return fmt % statistics.median(xs)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("results")
    ap.add_argument("--topology")
    a = ap.parse_args()
    profiles = {}
    if a.topology:
        for line in open(a.topology):
            f = line.split("#", 1)[0].split()
            if len(f) >= 2 and f[0] != "set":
                profiles[f[0]] = f[1]
    groups = {}
    order = []
    for line in open(a.results):
        if not line.strip():
            continue
        r = parse_line(line)
        key = (r.get("listener", "?"), r.get("sender", "?"))
        if key not in groups:
            groups[key] = []
            order.append(key)
        groups[key].append(r)

    def name(n):
        return "%s (%s)" % (n, profiles[n]) if n in profiles else n

    print("| listener | sender | connected | final path | t_direct s | first request ms | direct % | RTT direct/relay ms |")
    print("|---|---|---|---|---|---|---|---|")
    for key in order:
        rs = groups[key]
        ok = sum(1 for r in rs if r.get("ok") == "1")
        final = "".join({"direct": "d", "relay": "r"}.get(r.get("last_path"), "-") for r in rs)
        direct = [r.get("t_direct") for r in rs if r.get("t_direct") not in (None, "-")]
        tdir = med(direct, "%.2f")
        if direct and len(direct) < len(rs):
            tdir += " (%d/%d)" % (len(direct), len(rs))
        print(
            "| %s | %s | %d/%d | %s | %s | %s | %s | %s/%s |"
            % (
                name(key[0]),
                name(key[1]),
                ok,
                len(rs),
                final,
                tdir,
                med([r.get("first_ms") for r in rs], "%.0f"),
                med([r.get("direct_pct") for r in rs], "%.0f"),
                med([r.get("direct_ms") for r in rs], "%.0f"),
                med([r.get("relay_ms") for r in rs], "%.0f"),
            )
        )
    errs = [(k, r["error"]) for k in order for r in groups[k] if r.get("error", "-") != "-"]
    if errs:
        print()
        for k, e in errs:
            print("- %s -> %s: %s" % (k[0], k[1], e))


if __name__ == "__main__":
    sys.exit(main())
