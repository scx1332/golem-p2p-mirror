#!/usr/bin/env python3
"""Summarizes one `lab.sh pair` run.

Input: the sender's stdout with a unix timestamp prepended to every line
("<ts> endpoint=... echo=ping (74 ms, relay)"), its stderr, and the
listener's log. Output: one line of key=value pairs:

  ok           1 if at least one reply arrived
  replies      number of replies
  t_first      seconds from process start to the first reply (includes bind,
               which waits up to 5 s for the relay, the directory lookup and
               the connection)
  first_ms     duration of the first request as printed by send (lookup +
               connect + handshake + one request)
  first_path   path after the first request (direct/relay/unknown)
  t_direct     seconds from process start to the first reply on a direct path
               ("-" if never)
  last_path    path after the last request
  switches     number of path changes
  direct_pct   share of replies on a direct path
  relay_ms / direct_ms   median request duration per path
  error        last error line of send, if any
"""

import argparse
import re
import statistics

LINE = re.compile(r"\((\d+) ms, (\w+)\)\s*$")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--start", type=float, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--err")
    ap.add_argument("--listen")
    a = ap.parse_args()

    rows = []
    with open(a.out, errors="replace") as f:
        for line in f:
            ts, _, rest = line.rstrip("\n").partition(" ")
            m = LINE.search(rest)
            if m:
                rows.append((float(ts), int(m.group(1)), m.group(2)))

    res = {"ok": 1 if rows else 0, "replies": len(rows)}
    if rows:
        paths = [r[2] for r in rows]
        res["t_first"] = "%.2f" % (rows[0][0] - a.start)
        res["first_ms"] = rows[0][1]
        res["first_path"] = paths[0]
        direct = [r for r in rows if r[2] == "direct"]
        res["t_direct"] = "%.2f" % (direct[0][0] - a.start) if direct else "-"
        res["last_path"] = paths[-1]
        res["switches"] = sum(1 for x, y in zip(paths, paths[1:]) if x != y)
        res["direct_pct"] = round(100 * len(direct) / len(rows))
        for p in ("relay", "direct"):
            ms = [r[1] for r in rows[1:] if r[2] == p]
            res[p + "_ms"] = int(statistics.median(ms)) if ms else "-"
    err = "-"
    if a.err:
        try:
            with open(a.err, errors="replace") as f:
                lines = [l.strip() for l in f if l.strip()]
            errs = [l for l in lines if l.startswith("Error") or " ERROR " in l]
            if errs:
                err = errs[-1]
        except OSError:
            pass
    if a.listen:
        try:
            with open(a.listen, errors="replace") as f:
                via = [l.rsplit(" via ", 1)[1].split(":")[0] for l in f if " via " in l]
            res["listener_saw"] = ",".join("%s:%d" % (p, via.count(p)) for p in sorted(set(via))) or "-"
        except OSError:
            pass
    res["error"] = '"%s"' % err.replace('"', "'")[:200] if err != "-" else "-"
    print(" ".join("%s=%s" % (k, v) for k, v in res.items()))


if __name__ == "__main__":
    main()
