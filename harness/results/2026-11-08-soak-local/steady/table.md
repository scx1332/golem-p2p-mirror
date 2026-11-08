# Soak run steady

Scenario `steady`, load 2.506 h, warm-up 900.0 s; clients 100, workers 4, interval 1s, relay only False.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 32074.0 | 713928.0 | 1335532 | 1334160.0 | 111580.0 | 193386.972 | 0.791 | **yes** |
| cgroup-slice | mem_kb | 111064.0 | 1202152.0 | 2971132 | 1985902.0 | 401204.0 | 157614.174 | 0.044 |  |
| client* | rss_kb_sum | None | 2161162.0 | 2734576 | 2734308.0 | None | 171579.67 | 0.759 | **yes** |
| client* | anon_kb_sum | None | 611930.0 | 1191432 | 1191164.0 | None | 177496.792 | 0.773 | **yes** |
| client* | rss_kb_max | None | 21820.0 | 28084 | 28052.0 | None | 1586.602 | 0.544 |  |
| client* | fds_sum | None | 1200.0 | 1301 | 1200.0 | None | -0.554 | 0.0 |  |
| client* | threads_sum | None | 300.0 | 398 | 300.0 | None | -0.088 | 0.0 |  |
| client* | n | None | 100.0 | 100 | 100.0 | None | 0.0 | 1.0 |  |
| directory | rss_kb | 9616.0 | 11724.0 | 12940 | 12940.0 | 12940.0 | 453.097 | 0.666 |  |
| directory | anon_kb | 1268.0 | 3392.0 | 4804 | 4804.0 | 4804.0 | 521.301 | 0.705 |  |
| directory | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 10.0 | 110 | 10.0 | 10.0 | -0.567 | 0.0 |  |
| directory | sockets | 5.0 | 4.0 | 104 | 4.0 | 4.0 | -0.567 | 0.0 |  |
| listener | rss_kb | 20418.0 | 34994.0 | 52356 | 52356.0 | 52364.0 | 6180.614 | 0.905 | **yes** |
| listener | anon_kb | 4842.0 | 19782.0 | 36936 | 36936.0 | 36944.0 | 6199.804 | 0.904 | **yes** |
| listener | threads | 7.0 | 7.0 | 8 | 7.0 | 7.0 | -0.002 | 0.0 |  |
| listener | fds | 14.0 | 13.0 | 14 | 13.0 | 13.0 | -0.012 | 0.001 |  |
| listener | sockets | 7.0 | 6.0 | 7 | 6.0 | 6.0 | -0.012 | 0.001 |  |
| relay | rss_kb | 10404.0 | 25896.0 | 31376 | 31048.0 | 30900.0 | 1976.212 | 0.58 |  |
| relay | anon_kb | 1268.0 | 18376.0 | 23628 | 23508.0 | 23260.0 | 1813.506 | 0.64 | **yes** |
| relay | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 114.0 | 115 | 114.0 | 14.0 | 0.001 | 0.0 |  |
| relay | sockets | 8.0 | 108.0 | 109 | 108.0 | 8.0 | 0.001 | 0.0 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 16.0 | 716.0 | 721 | 715.0 | 15.0 | -1.233 | 0.378 |
| ep.connections | 0.0 | 100.0 | 100 | 100.0 | 0.0 | 0.0 | 1.0 |
| ep.open_connections | 0.0 | 100.0 | 100 | 100.0 | 0.0 | 0.0 | 1.0 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.queued_streams | 0.0 | 0.0 | 2 | 0.0 | 0.0 | 0.002 | 0.0 |
| ep.cached_identities | 0.0 | 100.0 | 100 | 100.0 | 100.0 | 0.0 | 1.0 |
| ep.cached_endpoints | 0.0 | 100.0 | 100 | 100.0 | 100.0 | 0.0 | 1.0 |
| proc.rss_kb | 20474.0 | 34994.0 | 52356 | 52356.0 | 52364.0 | 6200.619 | 0.906 |
| proc.anon_kb | 4850.0 | 19782.0 | 36936 | 36936.0 | 36944.0 | 6217.673 | 0.905 |
| proc.heap_used_kb | 4066.5 | 18321.5 | 21473 | 21456.0 | 6544.0 | 1097.815 | 0.826 |
| proc.heap_free_kb | 4211.5 | 5445.5 | 21141 | 20991.0 | 35903.0 | 5791.185 | 0.911 |
| proc.fds | 14.5 | 14.0 | 15 | 14.0 | 14.0 | -0.033 | 0.005 |
| proc.threads | 7.0 | 7.0 | 8 | 7.0 | 7.0 | 0.015 | 0.009 |

ep.connections_incoming: {'at_load_end': 100, 'last': 100}

ep.streams_accepted: {'at_load_end': 903025, 'last': 903025}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

Clients: {"bins": 31, "bin_s": 300, "totals": {"sent": 901525, "ok": 901525, "failed": 0, "bulk_ok": 1500, "bulk_failed": 0, "new_conns": 100}, "errors": {}, "clients": 100, "clients_with_final_event": 100, "overall_hist_p50_ms": 0.5, "overall_hist_p99_ms": 3.0, "overall_hist_p999_ms": 15.0, "first_bin": {"p50": 0.226, "p99": 6.068}, "last_bin": {"p50": 0.333, "p99": 1.694}, "p50_trend_ms_per_h": 0.034, "p50_trend_r2": 0.027, "p99_trend_ms_per_h": -0.4409, "p99_trend_r2": 0.04, "failure_rate": 0.0, "client_mem_slope_kb_per_h": {"median": 121.954, "max": 189.23, "min": 72.3, "growing": 0}, "client_alive_tasks_slope_per_h": {"median": -0.006, "max": 0.092}}

Flags:

* client* rss_kb_sum grows 171579.67/h (R2 0.759, 2161162.0 -> 2734308.0, peak 2734576; still growing in the second half (73952.413/h))
* client* anon_kb_sum grows 177496.792/h (R2 0.773, 611930.0 -> 1191164.0, peak 1191432; still growing in the second half (76387.218/h))
* listener rss_kb grows 6180.614/h (R2 0.905, 34994.0 -> 52356.0, peak 52356; still growing in the second half (3944.309/h))
* listener anon_kb grows 6199.804/h (R2 0.904, 19782.0 -> 36936.0, peak 36936; still growing in the second half (3980.318/h))
* relay anon_kb grows 1813.506/h (R2 0.64, 18376.0 -> 23508.0, peak 23628; levels off in the second half)
* listener proc.heap_used_kb grows 1097.815/h (R2 0.826, 18321.5 -> 21456.0, peak 21473; levels off in the second half)
