# Soak run steady-arena2

Scenario `steady`, load 1.172 h, warm-up 900.0 s; clients 100, workers 4, interval 1s, relay only False.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 27116.0 | 661612.0 | 965384 | 963986.0 | 77360.0 | 269813.621 | 0.766 | **yes** |
| cgroup-slice | mem_kb | 1844218.0 | 2503540.0 | 2974496 | 2941398.0 | 2054856.0 | 452397.942 | 0.874 | **yes** |
| client* | rss_kb_sum | None | 2141134.0 | 2422632 | 2422216.0 | None | 250204.309 | 0.72 | **yes** |
| client* | anon_kb_sum | None | 574582.0 | 858256 | 858168.0 | None | 248849.955 | 0.744 | **yes** |
| client* | rss_kb_max | None | 22380.0 | 25460 | 25460.0 | None | 2110.356 | 0.407 |  |
| client* | fds_sum | None | 1200.0 | 1300 | 1200.0 | None | -3.281 | 0.001 |  |
| client* | threads_sum | None | 300.0 | 381 | 300.0 | None | -0.469 | 0.0 |  |
| client* | n | None | 100.0 | 100 | 100.0 | None | 0.0 | 1.0 |  |
| directory | rss_kb | 9484.0 | 11684.0 | 12304 | 12300.0 | 12284.0 | 533.377 | 0.735 |  |
| directory | anon_kb | 1240.0 | 3296.0 | 4012 | 4008.0 | 4008.0 | 607.892 | 0.721 |  |
| directory | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 10.0 | 110 | 10.0 | 10.0 | -3.355 | 0.001 |  |
| directory | sockets | 5.0 | 4.0 | 104 | 4.0 | 4.0 | -3.355 | 0.001 |  |
| listener | rss_kb | 20408.0 | 33112.0 | 38736 | 38736.0 | 38784.0 | 4653.568 | 0.769 | **yes** |
| listener | anon_kb | 4824.0 | 17592.0 | 23180 | 23180.0 | 23208.0 | 4679.179 | 0.778 | **yes** |
| listener | threads | 7.0 | 7.0 | 8 | 7.0 | 7.0 | -0.01 | 0.001 |  |
| listener | fds | 14.0 | 13.0 | 14 | 13.0 | 13.0 | -0.075 | 0.005 |  |
| listener | sockets | 7.0 | 6.0 | 7 | 6.0 | 6.0 | -0.075 | 0.005 |  |
| relay | rss_kb | 10650.0 | 24278.0 | 27972 | 26996.0 | 27062.0 | 4938.468 | 0.897 | **yes** |
| relay | anon_kb | 1230.0 | 14858.0 | 18624 | 17656.0 | 17708.0 | 4995.967 | 0.901 | **yes** |
| relay | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 114.0 | 115 | 114.0 | 14.0 | -0.002 | 0.0 |  |
| relay | sockets | 8.0 | 108.0 | 109 | 108.0 | 8.0 | -0.002 | 0.0 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 16.0 | 718.0 | 721 | 716.0 | 15.0 | -3.425 | 0.521 |
| ep.connections | 0.0 | 100.0 | 100 | 100.0 | 0.0 | 0.0 | 1.0 |
| ep.open_connections | 0.0 | 100.0 | 100 | 100.0 | 0.0 | 0.0 | 1.0 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.queued_streams | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.cached_identities | 0.0 | 100.0 | 100 | 100.0 | 100.0 | 0.0 | 1.0 |
| ep.cached_endpoints | 0.0 | 100.0 | 100 | 100.0 | 100.0 | 0.0 | 1.0 |
| proc.rss_kb | 20456.0 | 33112.0 | 38736 | 38736.0 | 38784.0 | 4670.225 | 0.765 |
| proc.anon_kb | 4872.0 | 17592.0 | 23180 | 23180.0 | 23208.0 | 4692.15 | 0.775 |
| proc.heap_used_kb | 4073.5 | 18373.5 | 21345 | 20605.0 | 6311.5 | 2172.109 | 0.873 |
| proc.heap_free_kb | 3584.5 | 1481.5 | 4753 | 4722.0 | 19015.5 | 1623.818 | 0.334 |
| proc.fds | 14.5 | 14.0 | 15 | 14.0 | 14.0 | -0.104 | 0.008 |
| proc.threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |

ep.connections_incoming: {'at_load_end': 100, 'last': 100}

ep.streams_accepted: {'at_load_end': 421895, 'last': 421895}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

Clients: {"bins": 15, "bin_s": 300, "totals": {"sent": 421194, "ok": 421194, "failed": 0, "bulk_ok": 700, "bulk_failed": 0, "new_conns": 100}, "errors": {}, "clients": 100, "clients_with_final_event": 100, "overall_hist_p50_ms": 0.75, "overall_hist_p99_ms": 3.0, "overall_hist_p999_ms": 20.0, "first_bin": {"p50": 0.629, "p99": 44.106}, "last_bin": {"p50": 0.622, "p99": 5.891}, "p50_trend_ms_per_h": 0.3555, "p50_trend_r2": 0.576, "p99_trend_ms_per_h": 1.5914, "p99_trend_r2": 0.039, "failure_rate": 0.0, "client_mem_slope_kb_per_h": {"median": 363.855, "max": 560.346, "min": 165.611, "growing": 0}, "client_alive_tasks_slope_per_h": {"median": -0.037, "max": 0.443}}

Flags:

* client* rss_kb_sum grows 250204.309/h (R2 0.72, 2141134.0 -> 2422216.0, peak 2422632; still growing in the second half (128426.667/h))
* client* anon_kb_sum grows 248849.955/h (R2 0.744, 574582.0 -> 858168.0, peak 858256; still growing in the second half (128450.999/h))
* listener rss_kb grows 4653.568/h (R2 0.769, 33112.0 -> 38736.0, peak 38736; levels off in the second half)
* listener anon_kb grows 4679.179/h (R2 0.778, 17592.0 -> 23180.0, peak 23180; levels off in the second half)
* relay rss_kb grows 4938.468/h (R2 0.897, 24278.0 -> 26996.0, peak 27972; still growing in the second half (3057.935/h))
* relay anon_kb grows 4995.967/h (R2 0.901, 14858.0 -> 17656.0, peak 18624; still growing in the second half (3070.925/h))
* listener errors: {'connection': 1}
