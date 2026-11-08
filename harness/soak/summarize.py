#!/usr/bin/env python3
"""Summarizes a soak run directory (harness/soak/soak.sh) into
DIR/summary.json, DIR/table.md and plot data in DIR/plot/*.csv.

Usage: summarize.py DIR [DIR...]
       summarize.py --combine OUT_DIR RUN_DIR...   (key facts of several runs
                                                    in OUT_DIR/summary.json)

What it reads (all optional except phases.jsonl):
  phases.jsonl          idle, load_start, load_end, cooldown, end (epoch s)
  meta.json             the run's parameters
  samples.jsonl         sampler.py: per process / group / cgroup / host / prom
  listener.jsonl        `stats` events (endpoint state, process, tokio), errors
  directory.jsonl       `stats` events (store size, process, tokio)
  clients/*.jsonl       `soak_stats` events of the steady clients
  churn/*.jsonl         `soak_stats` (one per session), errors, session_exit

Growth analysis: for every series (RSS, anonymous memory, threads, file
descriptors, sockets of each process; tokio tasks and connection tables of
the listener; store size of the directory) it reports the idle baseline, the
value after warm-up, the peak, the value at the end of the load and after the
cool-down, and a least-squares slope over the load after warm-up (the first
quarter of the load, at most 15 minutes). A series is flagged as growing when
the fitted growth over that window exceeds a threshold (memory: 5 % of the
starting value and at least 2 MiB; fds and sockets: 4; threads: 2; tasks: 10 %
and at least 20) with R^2 >= 0.6. Memory is also related to the number of
connections the listener served (KiB per connection) and to the directory's
stored records (bytes per record), which separates growth with time from
growth with work. Latency percentiles over time are merged from the clients'
histograms (bucket upper bounds, so they are upper estimates); the trend of
the per-bin p50/p99 is fitted the same way.
"""

import csv
import glob
import json
import math
import os
import statistics
import sys
from collections import defaultdict

BIN_S = 300  # latency/churn time bins


def load_jsonl(path):
    """Events of a JSON lines file, also gzipped (`path` or `path.gz`)."""
    import gzip

    out = []
    if path.endswith(".gz"):
        opener = gzip.open
    elif not os.path.exists(path) and os.path.exists(path + ".gz"):
        path, opener = path + ".gz", gzip.open
    else:
        opener = open
    try:
        with opener(path, "rt") as f:
            for line in f:
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    except OSError:
        pass
    return out


def jsonl_files(d):
    """The *.jsonl and *.jsonl.gz files in d (one per name)."""
    names = {}
    for p in glob.glob(os.path.join(d, "*.jsonl")) + glob.glob(os.path.join(d, "*.jsonl.gz")):
        names.setdefault(p.removesuffix(".gz"), p)
    return sorted(names.values())


def ts_epoch(ts):
    """RFC 3339 UTC with milliseconds -> epoch seconds."""
    import calendar
    import time

    base, _, frac = ts.rstrip("Z").partition(".")
    t = calendar.timegm(time.strptime(base, "%Y-%m-%dT%H:%M:%S"))
    return t + (float("0." + frac) if frac else 0.0)


def fit(points):
    """Least squares y = a + b*x. Returns (slope, intercept, r2) or None."""
    if len(points) < 5:
        return None
    xs = [p[0] for p in points]
    ys = [p[1] for p in points]
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx == 0:
        return None
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    b = sxy / sxx
    a = my - b * mx
    syy = sum((y - my) ** 2 for y in ys)
    r2 = 1.0 if syy == 0 else (sxy * sxy) / (sxx * syy)
    return b, a, r2


def median_in(points, t0, t1):
    v = [y for t, y in points if t0 <= t < t1]
    return statistics.median(v) if v else None


def r(x, n=3):
    return None if x is None else round(x, n)


class Phases:
    def __init__(self, d):
        self.p = {}
        self.died = []
        for e in load_jsonl(os.path.join(d, "phases.jsonl")):
            if "phase" in e:
                self.p[e["phase"]] = float(e["ts"])
            elif e.get("event") == "died":
                self.died.append(e)
        self.idle = self.p.get("idle")
        self.load_start = self.p.get("load_start")
        self.load_end = self.p.get("load_end")
        self.end = self.p.get("end") or self.p.get("cooldown")
        if self.load_start and self.load_end:
            load = self.load_end - self.load_start
            self.warmup = min(900.0, load / 4)
            self.fit_start = self.load_start + self.warmup
        else:
            self.warmup = None
            self.fit_start = None

    def ok(self):
        return self.load_start is not None and self.load_end is not None


def thresholds(kind, start):
    start = abs(start or 0)
    if kind == "mem_kb":
        return max(2048.0, 0.05 * start)
    if kind in ("fds", "sockets"):
        return 4.0
    if kind == "threads":
        return 2.0
    if kind == "tasks":
        return max(20.0, 0.1 * start)
    return max(1.0, 0.1 * start)


def analyze(points, ph, kind):
    """Growth analysis of a series [(epoch, value)]."""
    points = sorted((t, v) for t, v in points if v is not None)
    if not points or not ph.ok():
        return None
    res = {
        "idle": r(median_in(points, ph.idle or 0, ph.load_start)),
        "start": r(median_in(points, ph.fit_start, ph.fit_start + 120)),
        "peak": max(v for t, v in points if ph.load_start <= t < ph.load_end + 1)
        if any(ph.load_start <= t < ph.load_end + 1 for t, _ in points)
        else None,
        "load_end": r(median_in(points, ph.load_end - 120, ph.load_end + 1)),
        "after_cooldown": r(median_in(points, (ph.end or ph.load_end) - 60, (ph.end or ph.load_end) + 1)),
        "last": points[-1][1],
    }
    window = [(t - ph.fit_start, v) for t, v in points if ph.fit_start <= t <= ph.load_end]
    f = fit(window)
    if f:
        slope_h = f[0] * 3600
        hours = (ph.load_end - ph.fit_start) / 3600
        growth = slope_h * hours
        thr = thresholds(kind, res["start"] if res["start"] is not None else window[0][1])
        # The second half of the window tells growth that goes on from
        # growth that levels off (caches filling, allocator warm-up).
        mid = (ph.load_end - ph.fit_start) / 2
        late = fit([(t, v) for t, v in window if t >= mid])
        late_growth = late[0] * 3600 * hours / 2 if late else None
        res.update(
            {
                "slope_per_h": r(slope_h),
                "r2": r(f[2]),
                "fit_hours": r(hours),
                "growth_in_fit": r(growth),
                "slope_second_half_per_h": r(late[0] * 3600) if late else None,
                "growth_second_half": r(late_growth),
                "threshold": r(thr),
                # The fit, and the values themselves, must both show growth.
                "growing": bool(
                    growth > thr
                    and f[2] >= 0.6
                    and res["start"] is not None
                    and res["load_end"] is not None
                    and res["load_end"] - res["start"] > thr / 2
                ),
            }
        )
        # Still growing in the second half (at least half the threshold
        # there), as opposed to leveling off.
        res["still_growing"] = bool(res["growing"] and late_growth is not None and late_growth > thr / 2)
    return res


def growth_flag(name, a):
    """The flag text of a growing series, saying whether it levels off."""
    trend = (f"still growing in the second half ({a.get('slope_second_half_per_h')}/h)"
             if a.get("still_growing") else "levels off in the second half")
    return (f"{name} grows {a['slope_per_h']}/h (R2 {a['r2']}, {a['start']} -> {a['load_end']}, "
            f"peak {a['peak']}; {trend})")


def per_unit(points_y, points_x, ph):
    """Slope of y against x (both [(epoch, value)]) over the fit window, by
    pairing each y sample with the latest x sample before it."""
    xs = sorted(points_x)
    if not xs or not ph.ok():
        return None
    pairs = []
    j = 0
    for t, y in sorted(points_y):
        if not (ph.fit_start <= t <= ph.load_end) or y is None:
            continue
        while j + 1 < len(xs) and xs[j + 1][0] <= t:
            j += 1
        if xs[j][0] <= t:
            pairs.append((xs[j][1], y))
    if len(pairs) < 5 or max(p[0] for p in pairs) == min(p[0] for p in pairs):
        return None
    f = fit(pairs)
    if not f:
        return None
    return {"slope": r(f[0], 5), "r2": r(f[2]), "x_range": [pairs[0][0], pairs[-1][0]]}


def hist_percentile(hist, p):
    """Upper bound of the bucket holding the p-th percentile."""
    items = sorted(((math.inf if k == "inf" else float(k)), c) for k, c in hist.items())
    total = sum(c for _, c in items)
    if total == 0:
        return None
    rank = math.ceil(p / 100 * total)
    acc = 0
    for b, c in items:
        acc += c
        if acc >= rank:
            return None if b == math.inf else b
    return None


def pct(vals, p):
    if not vals:
        return None
    s = sorted(vals)
    k = max(1, math.ceil(p / 100 * len(s)))
    return s[k - 1]


def summarize(d):
    ph = Phases(d)
    meta = {}
    try:
        meta = json.load(open(os.path.join(d, "meta.json")))
    except (OSError, ValueError):
        pass
    plot = os.path.join(d, "plot")
    os.makedirs(plot, exist_ok=True)
    flags = []
    summary = {
        "dir": os.path.basename(os.path.abspath(d)),
        "meta": meta,
        "phases": {k: v for k, v in ph.p.items()},
        "load_hours": r((ph.load_end - ph.load_start) / 3600) if ph.ok() else None,
        "warmup_s": ph.warmup,
        "died": ph.died,
    }
    t0 = ph.load_start or 0

    # --- sampler: processes, groups, cgroup, prom
    samples = load_jsonl(os.path.join(d, "samples.jsonl"))
    series = defaultdict(lambda: defaultdict(list))  # role -> metric -> [(t, v)]
    cpu = defaultdict(list)
    prom = defaultdict(list)
    pids = defaultdict(set)
    proc_rows = []
    for s in samples:
        t = ts_epoch(s["ts"])
        k = s.get("kind")
        if k == "proc":
            role = s["role"]
            pids[role].add(s["pid"])
            for m in ("rss_kb", "anon_kb", "threads", "fds", "sockets"):
                series[role][m].append((t, s.get(m)))
            cpu[role].append((t, s.get("cpu_s")))
            proc_rows.append([round((t - t0) / 60, 2), role, s.get("rss_kb"), s.get("anon_kb"),
                              s.get("threads"), s.get("fds"), s.get("sockets"), s.get("cpu_s")])
        elif k == "group":
            role = s["role"] + "*"
            series[role]["n"].append((t, s.get("n")))
            for m in ("rss_kb_sum", "rss_kb_max", "anon_kb_sum", "anon_kb_max", "threads_sum", "fds_sum", "sockets_sum"):
                series[role][m].append((t, s.get(m)))
            proc_rows.append([round((t - t0) / 60, 2), role, s.get("rss_kb_sum"), s.get("anon_kb_sum"),
                              s.get("threads_sum"), s.get("fds_sum"), s.get("sockets_sum"), s.get("cpu_s_sum")])
        elif k == "cgroup":
            for name in ("scope", "slice"):
                if name in s:
                    series["cgroup-" + name]["mem_kb"].append((t, s[name].get("mem_kb")))
        elif k == "prom":
            prom[s["name"]].append((t, s.get("metrics") or {}))
    with open(os.path.join(plot, "procs.csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["t_min", "role", "rss_kb", "anon_kb", "threads", "fds", "sockets", "cpu_s"])
        w.writerows(proc_rows)

    kinds = {
        "rss_kb": "mem_kb", "anon_kb": "mem_kb", "rss_kb_sum": "mem_kb", "rss_kb_max": "mem_kb",
        "anon_kb_sum": "mem_kb", "anon_kb_max": "mem_kb", "mem_kb": "mem_kb",
        "heap_used_kb": "mem_kb", "heap_free_kb": "mem_kb",
        "threads": "threads", "threads_sum": "threads", "fds": "fds", "fds_sum": "fds",
        "sockets": "sockets", "sockets_sum": "sockets", "n": "count",
    }
    procs = {}
    for role, ms in sorted(series.items()):
        out = {}
        for m, pts in ms.items():
            a = analyze(pts, ph, kinds.get(m, "count"))
            if a:
                out[m] = a
                # The churn group is too noisy to flag (processes come and
                # go), and cgroup memory includes the page cache of the
                # files the run writes.
                flaggable = not role.startswith(("churn", "cgroup")) and m != "n"
                if a.get("growing") and flaggable:
                    flags.append(growth_flag(f"{role} {m}", a))
        if role in cpu and ph.ok():
            c = sorted((t, v) for t, v in cpu[role] if v is not None)
            during = [(t, v) for t, v in c if ph.load_start <= t <= ph.load_end]
            if len(during) >= 2 and len(pids[role]) == 1:
                out["cpu_load_pct"] = r((during[-1][1] - during[0][1]) / (during[-1][0] - during[0][0]) * 100, 1)
            if len(pids[role]) > 1:
                out["restarted"] = sorted(pids[role])
                flags.append(f"{role} has several pids (restarted?): {sorted(pids[role])}")
        procs[role] = out
    summary["processes"] = procs

    # --- relay metrics
    relay = {}
    for name, rows in prom.items():
        rows = sorted(rows, key=lambda x: x[0])
        good = [(t, m) for t, m in rows if "_error" not in m]
        if not good:
            relay[name] = {"error": rows[-1][1].get("_error") if rows else "no data"}
            continue
        first, last = good[0][1], good[-1][1]
        at_end = [m for t, m in good if ph.load_end and t <= ph.load_end + 30]
        end = at_end[-1] if at_end else last
        relay[name] = {
            "samples": len(good),
            "at_load_end": {k: v for k, v in sorted(end.items()) if v},
            "delta_load": {k: v - first.get(k, 0) for k, v in sorted(end.items())
                           if isinstance(v, (int, float)) and v - first.get(k, 0)},
            "last": {k: v for k, v in sorted(last.items()) if v},
        }
    summary["prom"] = relay

    # --- listener / directory stats events
    def stats_series(path, prefix):
        evs = [e for e in load_jsonl(path) if e.get("event") == "stats"]
        ser = defaultdict(list)
        rows = []
        for e in evs:
            t = ts_epoch(e["ts"])
            flat = {}
            for k in ("alive_tasks", "workers", "global_queue"):
                flat["tokio." + k] = (e.get("tokio") or {}).get(k)
            for k, v in (e.get("endpoint") or {}).items():
                if isinstance(v, dict):
                    for k2, v2 in v.items():
                        flat[f"iroh.{k2}"] = v2
                else:
                    flat["ep." + k] = v
            for k, v in (e.get("store") or {}).items():
                flat["store." + k] = v
            for k in ("rss_kb", "anon_kb", "heap_used_kb", "heap_free_kb", "threads", "fds", "sockets", "cpu_s"):
                flat["proc." + k] = (e.get("proc") or {}).get(k)
            for k, v in flat.items():
                ser[k].append((t, v))
            rows.append((round((t - t0) / 60, 2), flat))
        if rows:
            keys = sorted({k for _, f in rows for k in f})
            with open(os.path.join(plot, f"{prefix}_stats.csv"), "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(["t_min"] + keys)
                for tm, fl in rows:
                    w.writerow([tm] + [fl.get(k) for k in keys])
        return ser, evs

    lser, levs = stats_series(os.path.join(d, "listener.jsonl"), "listener")
    dser, devs = stats_series(os.path.join(d, "directory.jsonl"), "directory")
    gauges = ["tokio.alive_tasks", "ep.connections", "ep.open_connections", "ep.connect_slots",
              "ep.dial_locks", "ep.handshakes", "ep.queued_streams", "ep.cached_identities",
              "ep.cached_endpoints", "proc.rss_kb", "proc.anon_kb", "proc.heap_used_kb",
              "proc.heap_free_kb", "proc.fds", "proc.threads"]
    totals = ["ep.connections_incoming", "ep.connections_closed", "ep.handshakes_failed",
              "ep.streams_accepted", "ep.streams_refused", "iroh.conns_opened", "iroh.conns_closed"]
    lst = {}
    for g in gauges:
        if g in lser:
            k = "tasks" if g.startswith("tokio") else kinds.get(g.split(".")[1], "count")
            lst[g] = analyze(lser[g], ph, k)
    for tname in totals:
        if tname in lser and lser[tname]:
            pts = sorted(lser[tname])
            during = [v for t, v in pts if ph.ok() and ph.load_start <= t <= ph.load_end + 30]
            lst[tname] = {"at_load_end": during[-1] if during else None, "last": pts[-1][1]}
    if lser.get("proc.rss_kb") and lser.get("ep.connections_incoming"):
        lst["rss_kb_per_connection"] = per_unit(lser["proc.rss_kb"], lser["ep.connections_incoming"], ph)
        lst["anon_kb_per_connection"] = per_unit(lser["proc.anon_kb"], lser["ep.connections_incoming"], ph)
        if any(v is not None for _, v in lser.get("proc.heap_used_kb", [])):
            lst["heap_used_kb_per_connection"] = per_unit(
                [(t, v) for t, v in lser["proc.heap_used_kb"] if v is not None], lser["ep.connections_incoming"], ph)
    for g in ("tokio.alive_tasks", "ep.connections", "proc.fds", "proc.heap_used_kb"):
        a = lst.get(g)
        if a and a.get("growing"):
            flags.append(growth_flag(f"listener {g}", a))
    # Gauges that should return to their idle value once the load is gone.
    for g, slack in (("ep.connections", 0), ("ep.handshakes", 0), ("ep.queued_streams", 0),
                     ("tokio.alive_tasks", 5), ("proc.fds", 2)):
        a = lst.get(g)
        if a and a.get("idle") is not None and a.get("after_cooldown") is not None:
            if a["after_cooldown"] > a["idle"] + slack:
                flags.append(f"listener {g} did not return to idle after the cool-down: "
                             f"{a['idle']} idle, {a['after_cooldown']} after")
    # Cost of an open connection: memory with the load's connections open
    # (after warm-up) minus idle, per open connection.
    conns = (lst.get("ep.open_connections") or {}).get("start")
    if conns:
        cost = {}
        for g in ("proc.heap_used_kb", "proc.anon_kb", "proc.rss_kb"):
            a = lst.get(g) or {}
            if a.get("start") is not None and a.get("idle") is not None:
                cost[g.split(".")[1]] = r((a["start"] - a["idle"]) / conns, 1)
        cost["open_connections"] = conns
        lst["kb_per_open_connection"] = cost
        relay_anon = (procs.get("relay") or {}).get("anon_kb") or {}
        if relay_anon.get("start") is not None and relay_anon.get("idle") is not None:
            summary["relay_anon_kb_per_listener_connection"] = r((relay_anon["start"] - relay_anon["idle"]) / conns, 1)
    summary["listener"] = lst
    dst = {}
    for g in ("store.identities", "store.endpoints", "proc.rss_kb", "proc.anon_kb", "proc.heap_used_kb",
              "proc.heap_free_kb", "proc.fds", "tokio.alive_tasks"):
        if g in dser:
            k = "tasks" if g.startswith("tokio") else kinds.get(g.split(".")[1], "count")
            dst[g] = analyze(dser[g], ph, k)
    if dser.get("proc.rss_kb") and dser.get("store.identities"):
        dst["rss_kb_per_identity"] = per_unit(dser["proc.rss_kb"], dser["store.identities"], ph)
        if any(v is not None for _, v in dser.get("proc.heap_used_kb", [])):
            dst["heap_used_kb_per_identity"] = per_unit(
                [(t, v) for t, v in dser["proc.heap_used_kb"] if v is not None], dser["store.identities"], ph)
    for g in ("proc.heap_used_kb", "tokio.alive_tasks"):
        a = dst.get(g)
        if a and a.get("growing"):
            flags.append(growth_flag(f"directory {g}", a))
    summary["directory"] = dst
    errors = defaultdict(int)
    for e in load_jsonl(os.path.join(d, "listener.jsonl")):
        if e.get("event") == "error":
            kind = e.get("kind", "?")
            # The requester gave up on the request (dropped it, timed out,
            # or exited): a cancellation, not a failure of the listener.
            # Codes: 10 TIMED_OUT, 11 CANCELLED, 0 a stream dropped unread
            # (CLIs before requests were reset with CANCELLED).
            msg = e.get("message", "")
            if kind == "stream_rejected" and any(f"(code {c})" in msg for c in (0, 10, 11)):
                kind = "cancelled"
            errors[kind] += 1
    summary["listener_errors"] = dict(errors)
    failures = {k: v for k, v in errors.items() if k != "cancelled"}
    if failures:
        flags.append(f"listener errors: {failures}")

    # --- steady clients
    bins = defaultdict(lambda: {"hist": defaultdict(int), "p50s": [], "p99s": [], "sent": 0, "ok": 0,
                                "failed": 0, "errors": defaultdict(int), "bulk_ok": 0, "bulk_failed": 0,
                                "mbps": [], "new_conns": 0})
    client_errors = defaultdict(int)
    client_proc = defaultdict(list)
    client_files = jsonl_files(os.path.join(d, "clients"))
    totals_c = {"sent": 0, "ok": 0, "failed": 0, "bulk_ok": 0, "bulk_failed": 0, "new_conns": 0}
    clients_final = 0
    for path in client_files:
        evs = load_jsonl(path)
        last_tot = None
        for e in evs:
            if e.get("event") != "soak_stats":
                continue
            t = ts_epoch(e["ts"])
            b = int((t - t0) // BIN_S) if t0 else 0
            x = bins[b]
            for k, v in (e.get("rtt_hist") or {}).items():
                x["hist"][k] += v
            rt = e.get("rtt_ms") or {}
            if rt.get("p50") is not None:
                x["p50s"].append(rt["p50"])
                x["p99s"].append(rt["p99"])
            for k in ("sent", "ok", "failed", "new_conns"):
                x[k] += e.get(k, 0)
            for k, v in (e.get("errors") or {}).items():
                x["errors"][k] += v
                client_errors[k] += v
            bl = e.get("bulk") or {}
            x["bulk_ok"] += bl.get("ok", 0)
            x["bulk_failed"] += bl.get("failed", 0)
            if bl.get("mbps"):
                x["mbps"].append(bl["mbps"])
            last_tot = e.get("totals")
            if e.get("final"):
                clients_final += 1
            p = e.get("proc") or {}
            if p.get("rss_kb"):
                client_proc[os.path.basename(path)].append((t, p.get("rss_kb"), p.get("anon_kb"), p.get("fds"),
                                                            p.get("threads"), (e.get("tokio") or {}).get("alive_tasks"),
                                                            p.get("heap_used_kb")))
        if last_tot:
            for k in totals_c:
                totals_c[k] += last_tot.get(k, 0)
    lat_rows = []
    lat_p50, lat_p99 = [], []
    for b in sorted(bins):
        x = bins[b]
        p50 = hist_percentile(x["hist"], 50)
        p99 = hist_percentile(x["hist"], 99)
        tm = b * BIN_S / 60
        lat_rows.append([tm, sum(x["hist"].values()), p50, hist_percentile(x["hist"], 90), p99,
                         r(statistics.median(x["p50s"])) if x["p50s"] else None,
                         r(max(x["p99s"])) if x["p99s"] else None,
                         x["sent"], x["ok"], x["failed"], json.dumps(dict(x["errors"])),
                         x["bulk_ok"], x["bulk_failed"],
                         r(statistics.median(x["mbps"]), 1) if x["mbps"] else None, x["new_conns"]])
        # Bins fully inside the fit window, for the trend.
        if ph.ok() and t0 + b * BIN_S >= ph.fit_start and t0 + (b + 1) * BIN_S <= ph.load_end:
            if x["p50s"]:
                lat_p50.append((b * BIN_S, statistics.median(x["p50s"])))
                lat_p99.append((b * BIN_S, statistics.median(x["p99s"])))
    if lat_rows:
        with open(os.path.join(plot, "latency.csv"), "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["t_min", "samples", "hist_p50_ms", "hist_p90_ms", "hist_p99_ms",
                        "median_client_p50_ms", "max_client_p99_ms", "sent", "ok", "failed", "errors",
                        "bulk_ok", "bulk_failed", "bulk_mbps_median", "new_conns"])
            w.writerows(lat_rows)
    lat = None
    if client_files:
        lat = {"bins": len(lat_rows), "bin_s": BIN_S, "totals": totals_c, "errors": dict(client_errors),
               "clients": len(client_files), "clients_with_final_event": clients_final}
        allh = defaultdict(int)
        for x in bins.values():
            for k, v in x["hist"].items():
                allh[k] += v
        lat["overall_hist_p50_ms"] = hist_percentile(allh, 50)
        lat["overall_hist_p99_ms"] = hist_percentile(allh, 99)
        lat["overall_hist_p999_ms"] = hist_percentile(allh, 99.9)
        if lat_rows:
            lat["first_bin"] = {"p50": lat_rows[0][5], "p99": lat_rows[0][6]}
            lat["last_bin"] = {"p50": lat_rows[-1][5], "p99": lat_rows[-1][6]}
        for name, pts in (("p50", lat_p50), ("p99", lat_p99)):
            f = fit(pts)
            if f:
                lat[f"{name}_trend_ms_per_h"] = r(f[0] * 3600, 4)
                lat[f"{name}_trend_r2"] = r(f[2])
                start = pts[0][1]
                if f[0] * 3600 * (pts[-1][0] - pts[0][0]) / 3600 > max(0.5, 0.25 * start) and f[2] >= 0.6:
                    flags.append(f"client latency {name} grows {r(f[0] * 3600, 3)} ms/h (R2 {r(f[2])})")
        if totals_c["sent"]:
            lat["failure_rate"] = r(totals_c["failed"] / totals_c["sent"], 6)
        # Per-client memory: growth of each long-running client after warm-up.
        slopes, growing = [], 0
        for name, pts in client_proc.items():
            # Heap in use where reported (a leak), else anonymous memory.
            a = analyze([(t, heap if heap is not None else anon) for t, _, anon, _, _, _, heap in pts],
                        ph, "mem_kb")
            if a and a.get("slope_per_h") is not None:
                slopes.append(a["slope_per_h"])
                growing += bool(a.get("growing"))
        if slopes:
            lat["client_mem_slope_kb_per_h"] = {"median": r(statistics.median(slopes)), "max": r(max(slopes)),
                                                 "min": r(min(slopes)), "growing": growing}
            if growing > len(slopes) / 2:
                flags.append(f"steady client memory grows in {growing} of {len(slopes)} clients: "
                             f"median {r(statistics.median(slopes))} KiB/h")
        alive = []
        for name, pts in client_proc.items():
            fitpts = [(t, at) for t, _, _, _, _, at, _ in pts if at is not None]
            a = analyze(fitpts, ph, "tasks")
            if a and a.get("slope_per_h") is not None:
                alive.append(a["slope_per_h"])
        if alive:
            lat["client_alive_tasks_slope_per_h"] = {"median": r(statistics.median(alive)), "max": r(max(alive))}
    summary["clients"] = lat

    # --- churn
    churn_files = jsonl_files(os.path.join(d, "churn"))
    if churn_files:
        sess = []
        exits = defaultdict(int)
        cerr = defaultdict(int)
        cbins = defaultdict(lambda: {"n": 0, "fail": 0, "bind": [], "first": [], "fresh": 0, "reuse": 0})
        for path in churn_files:
            for e in load_jsonl(path):
                ev = e.get("event")
                if ev == "session_exit":
                    t = float(e["ts"])
                    exits[e.get("code")] += 1
                    b = int((t - t0) // BIN_S)
                    cbins[b]["n"] += 1
                    cbins[b]["reuse" if e.get("reuse") else "fresh"] += 1
                    if e.get("code") != 0:
                        cbins[b]["fail"] += 1
                elif ev == "soak_stats" and e.get("final"):
                    t = ts_epoch(e["ts"])
                    s = e.get("session") or {}
                    b = int((t - t0) // BIN_S)
                    if s.get("bind_ms") is not None:
                        cbins[b]["bind"].append(s["bind_ms"])
                    if s.get("first_ok_ms") is not None:
                        cbins[b]["first"].append(s["first_ok_ms"])
                    sess.append(e)
                elif ev == "error":
                    cerr[e.get("kind", "?")] += 1
        rows = []
        bind_trend, first_trend = [], []
        for b in sorted(cbins):
            x = cbins[b]
            rows.append([b * BIN_S / 60, x["n"], x["fail"], x["fresh"], x["reuse"],
                         pct(x["bind"], 50), pct(x["bind"], 99), pct(x["first"], 50), pct(x["first"], 99)])
            if ph.ok() and t0 + b * BIN_S >= ph.fit_start and t0 + (b + 1) * BIN_S <= ph.load_end:
                if x["bind"]:
                    bind_trend.append((b * BIN_S, statistics.median(x["bind"])))
                if x["first"]:
                    first_trend.append((b * BIN_S, statistics.median(x["first"])))
        with open(os.path.join(plot, "churn.csv"), "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["t_min", "sessions", "failed", "fresh", "reuse", "bind_p50_ms", "bind_p99_ms",
                        "first_ok_p50_ms", "first_ok_p99_ms"])
            w.writerows(rows)
        nsess = sum(exits.values())
        ch = {
            "sessions": nsess,
            "exit_codes": {str(k): v for k, v in sorted(exits.items(), key=lambda x: str(x[0]))},
            "errors": dict(cerr),
            "sessions_per_min": r(nsess / ((ph.load_end - ph.load_start) / 60), 2) if ph.ok() else None,
            "requests": sum((e.get("totals") or {}).get("sent", 0) for e in sess),
            "requests_failed": sum((e.get("totals") or {}).get("failed", 0) for e in sess),
            "bind_ms": {"p50": pct([b for x in cbins.values() for b in x["bind"]], 50),
                        "p99": pct([b for x in cbins.values() for b in x["bind"]], 99)},
            "first_ok_ms": {"p50": pct([b for x in cbins.values() for b in x["first"]], 50),
                            "p99": pct([b for x in cbins.values() for b in x["first"]], 99)},
        }
        for name, pts in (("bind_ms", bind_trend), ("first_ok_ms", first_trend)):
            f = fit(pts)
            if f:
                ch[f"{name}_trend_per_h"] = r(f[0] * 3600, 3)
                ch[f"{name}_trend_r2"] = r(f[2])
                if f[0] * 3600 * max(1e-9, (pts[-1][0] - pts[0][0]) / 3600) > max(20, 0.25 * pts[0][1]) and f[2] >= 0.6:
                    flags.append(f"churn {name} grows {r(f[0] * 3600, 1)} ms/h (R2 {r(f[2])})")
        if nsess and exits.get(0, 0) < nsess:
            ch["failed_sessions"] = nsess - exits.get(0, 0)
        summary["churn"] = ch

    summary["flags"] = flags
    with open(os.path.join(d, "summary.json"), "w") as f:
        json.dump(summary, f, indent=1, default=str)
    with open(os.path.join(d, "table.md"), "w") as f:
        f.write(table(summary))
    return summary


def table(s):
    lines = [f"# Soak run {s['dir']}", ""]
    m = s.get("meta") or {}
    lines.append(f"Scenario `{m.get('scenario')}`, load {s.get('load_hours')} h, warm-up {s.get('warmup_s')} s; "
                 f"clients {m.get('clients')}, workers {m.get('workers')}, interval {m.get('interval')}, "
                 f"relay only {m.get('relay_only')}.")
    lines.append("")
    lines.append("| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    for role, ms in sorted((s.get("processes") or {}).items()):
        for metric in ("rss_kb", "anon_kb", "threads", "fds", "sockets", "rss_kb_sum", "anon_kb_sum",
                       "rss_kb_max", "fds_sum", "threads_sum", "mem_kb", "n"):
            a = ms.get(metric)
            if not a:
                continue
            lines.append(f"| {role} | {metric} | {a.get('idle')} | {a.get('start')} | {a.get('peak')} | "
                         f"{a.get('load_end')} | {a.get('after_cooldown')} | {a.get('slope_per_h')} | "
                         f"{a.get('r2')} | {'**yes**' if a.get('growing') else ''} |")
    lst = s.get("listener") or {}
    if lst:
        lines += ["", "Listener internals:", "", "| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |",
                  "|---|---|---|---|---|---|---|---|"]
        for k, a in lst.items():
            if isinstance(a, dict) and "peak" in a:
                lines.append(f"| {k} | {a.get('idle')} | {a.get('start')} | {a.get('peak')} | {a.get('load_end')} | "
                             f"{a.get('after_cooldown')} | {a.get('slope_per_h')} | {a.get('r2')} |")
        for k in ("ep.connections_incoming", "ep.streams_accepted", "ep.handshakes_failed", "ep.streams_refused"):
            if k in lst:
                lines.append(f"\n{k}: {lst[k]}")
        for k in ("rss_kb_per_connection", "anon_kb_per_connection", "heap_used_kb_per_connection"):
            if lst.get(k):
                lines.append(f"\n{k}: {lst[k]}")
    c = s.get("clients")
    if c:
        lines += ["", f"Clients: {json.dumps(c)}"]
    ch = s.get("churn")
    if ch:
        lines += ["", f"Churn: {json.dumps(ch)}"]
    lines += ["", "Flags:", ""]
    lines += [f"* {f}" for f in s.get("flags") or ["none"]]
    return "\n".join(lines) + "\n"


def key_facts(s):
    """The headline numbers of one run's summary, for the combined summary."""
    keep = ("idle", "start", "peak", "load_end", "after_cooldown", "slope_per_h", "r2", "growing")
    out = {"scenario": (s.get("meta") or {}).get("scenario"), "load_hours": s.get("load_hours"),
           "flags": s.get("flags"), "processes": {}}
    for role, ms in (s.get("processes") or {}).items():
        if role.startswith("cgroup"):
            continue
        f = {}
        for m in ("rss_kb", "anon_kb", "rss_kb_sum", "anon_kb_sum", "fds", "threads", "fds_sum", "threads_sum"):
            if ms.get(m):
                f[m] = {k: ms[m].get(k) for k in keep}
        if "cpu_load_pct" in ms:
            f["cpu_load_pct"] = ms["cpu_load_pct"]
        out["processes"][role] = f
    wanted = ("proc.heap_used_kb", "proc.heap_free_kb", "tokio.alive_tasks", "ep.connections",
              "ep.cached_identities", "ep.cached_endpoints", "kb_per_open_connection",
              "heap_used_kb_per_connection", "ep.connections_incoming", "ep.streams_accepted")
    out["listener"] = {k: ({kk: v.get(kk) for kk in keep} if isinstance(v, dict) and "peak" in v else v)
                       for k, v in (s.get("listener") or {}).items() if k in wanted}
    out["directory"] = {k: v for k, v in (s.get("directory") or {}).items()
                        if k in ("proc.heap_used_kb", "store.identities", "heap_used_kb_per_identity")}
    for k in ("clients", "churn", "listener_errors", "relay_anon_kb_per_listener_connection"):
        if s.get(k) is not None:
            out[k] = s[k]
    prom = (s.get("prom") or {}).get("relay") or {}
    if prom.get("delta_load"):
        out["relay_metrics_delta"] = prom["delta_load"]
    return out


def combine(out_dir, dirs):
    """Writes OUT_DIR/summary.json with the key facts of several runs."""
    runs = {}
    for d in dirs:
        try:
            s = json.load(open(os.path.join(d, "summary.json")))
        except (OSError, ValueError):
            s = summarize(d)
        runs[os.path.basename(os.path.abspath(d))] = key_facts(s)
    with open(os.path.join(out_dir, "summary.json"), "w") as f:
        json.dump({"runs": runs}, f, indent=1, default=str)
    return runs


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    if args[0] == "--combine":
        if len(args) < 3:
            print("usage: summarize.py --combine OUT_DIR RUN_DIR...")
            return 2
        for name, k in combine(args[1], args[2:]).items():
            print(f"{name}: flags: {len(k['flags'] or [])}")
        return 0
    for d in args:
        s = summarize(d)
        print(f"{d}: flags: {len(s['flags'])}")
        for f in s["flags"]:
            print("  " + f)
    return 0


if __name__ == "__main__":
    sys.exit(main())
