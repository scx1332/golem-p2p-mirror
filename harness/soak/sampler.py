#!/usr/bin/env python3
"""Samples the resources of the processes of a soak run, one JSON line each.

Every --interval seconds it reads /proc for every process that belongs to the
run and writes to --out:

* one line per process of a *server* role (relay, directory, listener, ...):
  {"ts", "t", "kind": "proc", "role", "pid", "rss_kb", "hwm_kb", "anon_kb",
   "threads", "fds", "sockets", "cpu_s"}
* one line per *aggregated* role (roles named <prefix>-<number>, e.g. the
  clients client-1..client-60 or the churn workers churn-1..churn-4, are
  aggregated under <prefix>): {"kind": "group", "role", "n", "rss_kb_sum",
  "rss_kb_max", "anon_kb_sum", "anon_kb_max", "threads_sum", "fds_sum",
  "sockets_sum", "cpu_s_sum"}
* {"kind": "cgroup", ...}: memory and CPU of the cgroup the sampler runs in
  and of its parent (the systemd slice), if it is a systemd scope;
* {"kind": "host", "load1", "mem_available_kb"};
* every --prom-every samples, {"kind": "prom", "name", "metrics": {...}} with
  the numeric values of each --prom NAME=URL Prometheus endpoint (e.g. the
  relay's metrics).

A process belongs to the run if its command line contains `--run-id RUN`
(the golem-p2p CLI; its role is the value of `--node-name`), or if it
matches a --match ROLE=SUBSTRING (e.g. the relay's config path). `t` is
seconds since the sampler started. CPU time is user + system of the
processes alive at the sample (CPU of exited churn processes is not counted).

Stops on SIGTERM/SIGINT, after --duration seconds, or when --stop-file
appears. No dependencies beyond the Python standard library.
"""

import argparse
import json
import os
import re
import signal
import sys
import time
import urllib.request

CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
AGG = re.compile(r"^(.*)-\d+$")

stopping = False


def on_signal(_sig, _frame):
    global stopping
    stopping = True


def read(path):
    try:
        with open(path, "rb") as f:
            return f.read()
    except OSError:
        return None


def proc_info(pid):
    """Resources of one process, or None if it is gone."""
    status = read(f"/proc/{pid}/status")
    stat = read(f"/proc/{pid}/stat")
    if status is None or stat is None:
        return None
    fields = {}
    for line in status.decode(errors="replace").splitlines():
        k, _, v = line.partition(":")
        if k in ("VmRSS", "VmHWM", "RssAnon", "Threads"):
            try:
                fields[k] = int(v.split()[0])
            except (ValueError, IndexError):
                pass
    s = stat.decode(errors="replace")
    rest = s[s.rfind(")") + 2 :].split()
    try:
        cpu = (int(rest[11]) + int(rest[12])) / CLK_TCK
    except (ValueError, IndexError):
        cpu = None
    fds = sockets = None
    try:
        names = os.listdir(f"/proc/{pid}/fd")
        fds = len(names)
        sockets = 0
        for n in names:
            try:
                if os.readlink(f"/proc/{pid}/fd/{n}").startswith("socket:"):
                    sockets += 1
            except OSError:
                pass
    except OSError:
        pass
    return {
        "rss_kb": fields.get("VmRSS"),
        "hwm_kb": fields.get("VmHWM"),
        "anon_kb": fields.get("RssAnon"),
        "threads": fields.get("Threads"),
        "fds": fds,
        "sockets": sockets,
        "cpu_s": round(cpu, 2) if cpu is not None else None,
    }


def cmdline(pid):
    raw = read(f"/proc/{pid}/cmdline")
    if not raw:
        return None
    return [a.decode(errors="replace") for a in raw.split(b"\0") if a]


def arg_value(args, name):
    for i, a in enumerate(args):
        if a == name and i + 1 < len(args):
            return args[i + 1]
        if a.startswith(name + "="):
            return a[len(name) + 1 :]
    return None


def find_processes(run, matches, me):
    """{pid: role} of the processes of the run."""
    found = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        if pid == me:
            continue
        args = cmdline(pid)
        if not args:
            continue
        # Only the process itself, not a wrapper (timeout, a shell) whose
        # arguments contain the same command line.
        exe = os.path.basename(args[0])
        if run and arg_value(args, "--run-id") == run and exe.startswith("golem-p2p"):
            found[pid] = arg_value(args, "--node-name") or "golem-p2p"
            continue
        joined = " ".join(args)
        for role, sub in matches:
            if sub in joined and not exe.startswith(("bash", "sh", "timeout", "systemd-run")):
                found[pid] = role
                break
    return found


def cgroup_stats():
    raw = read("/proc/self/cgroup")
    if not raw:
        return None
    rel = None
    for line in raw.decode().splitlines():
        if line.startswith("0::"):
            rel = line[3:]
    if not rel:
        return None
    out = {}
    for name, path in (("scope", rel), ("slice", os.path.dirname(rel))):
        base = "/sys/fs/cgroup" + path
        mem = read(base + "/memory.current")
        cpu = read(base + "/cpu.stat")
        if mem is None:
            continue
        d = {"path": path, "mem_kb": int(mem) // 1024}
        if cpu:
            for line in cpu.decode().splitlines():
                k, _, v = line.partition(" ")
                if k == "usage_usec":
                    d["cpu_s"] = round(int(v) / 1e6, 2)
        out[name] = d
    return out


def host_stats():
    d = {}
    la = read("/proc/loadavg")
    if la:
        d["load1"] = float(la.split()[0])
    mi = read("/proc/meminfo")
    if mi:
        for line in mi.decode().splitlines():
            if line.startswith("MemAvailable:"):
                d["mem_available_kb"] = int(line.split()[1])
    return d


PROM_LINE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*(?:\{[^}]*\})?)\s+(\S+)")


def scrape(url, limit=300):
    try:
        with urllib.request.urlopen(url, timeout=5) as r:
            text = r.read(2_000_000).decode(errors="replace")
    except Exception as e:  # noqa: BLE001 - any failure is reported, not fatal
        return {"_error": str(e)}
    metrics = {}
    for line in text.splitlines():
        if line.startswith("#"):
            continue
        m = PROM_LINE.match(line)
        if not m or "_bucket{" in m.group(1):
            continue
        try:
            v = float(m.group(2))
        except ValueError:
            continue
        metrics[m.group(1)] = int(v) if v.is_integer() else v
        if len(metrics) >= limit:
            break
    return metrics


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", required=True, help="JSONL file to append to")
    ap.add_argument("--interval", type=float, default=10.0, help="seconds between samples")
    ap.add_argument("--run", help="run ID: sample golem-p2p processes with --run-id RUN")
    ap.add_argument(
        "--match",
        action="append",
        default=[],
        metavar="ROLE=SUBSTRING",
        help="also sample processes whose command line contains SUBSTRING, as ROLE",
    )
    ap.add_argument("--prom", action="append", default=[], metavar="NAME=URL")
    ap.add_argument("--prom-every", type=int, default=6, help="scrape every N samples")
    ap.add_argument("--duration", type=float, help="stop after this many seconds")
    ap.add_argument("--stop-file", help="stop when this file exists")
    args = ap.parse_args()
    matches = [tuple(m.split("=", 1)) for m in args.match]
    proms = [tuple(p.split("=", 1)) for p in args.prom]

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)
    me = os.getpid()
    start = time.time()
    n = 0
    with open(args.out, "a", buffering=1) as out:
        while not stopping:
            now = time.time()
            ts = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(now)) + f".{int(now % 1 * 1000):03d}Z"
            t = round(now - start, 1)
            lines = []
            groups = {}
            for pid, role in sorted(find_processes(args.run, matches, me).items()):
                info = proc_info(pid)
                if info is None:
                    continue
                m = AGG.match(role)
                if m:
                    g = groups.setdefault(m.group(1), [])
                    g.append(info)
                    continue
                lines.append({"ts": ts, "t": t, "kind": "proc", "role": role, "pid": pid, **info})
            for role, infos in sorted(groups.items()):

                def tot(k, infos=infos):
                    return sum(i[k] or 0 for i in infos)

                def mx(k, infos=infos):
                    return max((i[k] or 0) for i in infos)

                lines.append(
                    {
                        "ts": ts,
                        "t": t,
                        "kind": "group",
                        "role": role,
                        "n": len(infos),
                        "rss_kb_sum": tot("rss_kb"),
                        "rss_kb_max": mx("rss_kb"),
                        "anon_kb_sum": tot("anon_kb"),
                        "anon_kb_max": mx("anon_kb"),
                        "threads_sum": tot("threads"),
                        "fds_sum": tot("fds"),
                        "sockets_sum": tot("sockets"),
                        "cpu_s_sum": round(tot("cpu_s"), 2),
                    }
                )
            cg = cgroup_stats()
            if cg:
                lines.append({"ts": ts, "t": t, "kind": "cgroup", **cg})
            lines.append({"ts": ts, "t": t, "kind": "host", **host_stats()})
            if proms and n % max(args.prom_every, 1) == 0:
                for name, url in proms:
                    lines.append({"ts": ts, "t": t, "kind": "prom", "name": name, "metrics": scrape(url)})
            for line in lines:
                out.write(json.dumps(line, separators=(",", ":")) + "\n")
            n += 1
            if args.duration and time.time() - start >= args.duration:
                break
            if args.stop_file and os.path.exists(args.stop_file):
                break
            # Sleep until the next sample, waking up early on a signal.
            deadline = start + n * args.interval
            while not stopping and time.time() < deadline:
                time.sleep(min(0.5, max(0.0, deadline - time.time())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
