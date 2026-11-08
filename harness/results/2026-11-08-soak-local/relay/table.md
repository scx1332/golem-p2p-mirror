# Soak run relay

Scenario `relay`, load 2.504 h, warm-up 900.0 s; clients 50, workers 4, interval 1s, relay only True.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 24686.0 | 347248.0 | 458780 | 457572.0 | 73702.0 | 38666.107 | 0.837 | **yes** |
| cgroup-slice | mem_kb | 134788.0 | 1201576.0 | 2971752 | 1985216.0 | 400910.0 | 156076.671 | 0.043 |  |
| client* | rss_kb_sum | None | 1059120.0 | 1108344 | 1107976.0 | None | 9466.865 | 0.394 |  |
| client* | anon_kb_sum | None | 289464.0 | 367552 | 367184.0 | None | 22706.883 | 0.653 | **yes** |
| client* | rss_kb_max | None | 21416.0 | 22660 | 22620.0 | None | 198.819 | 0.287 |  |
| client* | fds_sum | None | 550.0 | 601 | 550.0 | None | -0.276 | 0.0 |  |
| client* | threads_sum | None | 150.0 | 199 | 150.0 | None | 0.0 | 1.0 |  |
| client* | n | None | 50.0 | 50 | 50.0 | None | 0.0 | 1.0 |  |
| directory | rss_kb | 9672.0 | 10764.0 | 11384 | 11276.0 | 11276.0 | 162.689 | 0.665 |  |
| directory | anon_kb | 1268.0 | 2316.0 | 3112 | 3032.0 | 3032.0 | 247.782 | 0.76 |  |
| directory | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 10.0 | 60 | 10.0 | 10.0 | -0.279 | 0.0 |  |
| directory | sockets | 5.0 | 4.0 | 54 | 4.0 | 4.0 | -0.279 | 0.0 |  |
| listener | rss_kb | 19828.0 | 29028.0 | 44228 | 44192.0 | 44192.0 | 8389.623 | 0.802 | **yes** |
| listener | anon_kb | 4806.0 | 13908.0 | 29628 | 29628.0 | 29628.0 | 8647.378 | 0.816 | **yes** |
| listener | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| listener | fds | 13.0 | 12.0 | 14 | 12.0 | 12.0 | -0.013 | 0.001 |  |
| listener | sockets | 6.0 | 5.0 | 7 | 5.0 | 5.0 | -0.013 | 0.001 |  |
| relay | rss_kb | 9804.0 | 12684.0 | 17028 | 16324.0 | 15276.0 | 1320.282 | 0.651 | **yes** |
| relay | anon_kb | 1052.0 | 5508.0 | 9100 | 9100.0 | 7936.0 | 1161.368 | 0.734 | **yes** |
| relay | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 64.0 | 67 | 64.0 | 14.0 | -0.009 | 0.0 |  |
| relay | sockets | 8.0 | 58.0 | 61 | 58.0 | 8.0 | -0.009 | 0.0 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 14.0 | 363.0 | 371 | 364.0 | 13.0 | -0.016 | 0.0 |
| ep.connections | 0.0 | 50.0 | 50 | 50.0 | 0.0 | 0.0 | 1.0 |
| ep.open_connections | 0.0 | 50.0 | 50 | 50.0 | 0.0 | 0.0 | 1.0 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.queued_streams | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.cached_identities | 0.0 | 50.0 | 50 | 50.0 | 50.0 | 0.0 | 1.0 |
| ep.cached_endpoints | 0.0 | 50.0 | 50 | 50.0 | 50.0 | 0.0 | 1.0 |
| proc.rss_kb | 19840.0 | 29028.0 | 44228 | 44192.0 | 44192.0 | 8402.684 | 0.802 |
| proc.anon_kb | 4892.0 | 13908.0 | 29628 | 29628.0 | 29628.0 | 8659.803 | 0.816 |
| proc.heap_used_kb | 978.0 | 8956.0 | 11521 | 11515.5 | 2769.5 | 636.384 | 0.689 |
| proc.heap_free_kb | 3177.0 | 4168.5 | 17743 | 17351.5 | 26097.5 | 8039.984 | 0.788 |
| proc.fds | 13.5 | 13.0 | 14 | 13.0 | 13.0 | -0.012 | 0.001 |
| proc.threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |

ep.connections_incoming: {'at_load_end': 50, 'last': 50}

ep.streams_accepted: {'at_load_end': 451330, 'last': 451330}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

Clients: {"bins": 31, "bin_s": 300, "totals": {"sent": 450580, "ok": 450580, "failed": 0, "bulk_ok": 747, "bulk_failed": 3, "new_conns": 50}, "errors": {"bulk": 3}, "clients": 50, "clients_with_final_event": 50, "overall_hist_p50_ms": 1.0, "overall_hist_p99_ms": 5.0, "overall_hist_p999_ms": 50.0, "first_bin": {"p50": 0.386, "p99": 7.857}, "last_bin": {"p50": 0.53, "p99": 9.579}, "p50_trend_ms_per_h": 0.0385, "p50_trend_r2": 0.014, "p99_trend_ms_per_h": -0.594, "p99_trend_r2": 0.034, "failure_rate": 0.0, "client_mem_slope_kb_per_h": {"median": 64.484, "max": 106.53, "min": 41.897, "growing": 0}, "client_alive_tasks_slope_per_h": {"median": 0.025, "max": 0.194}}

Flags:

* client* anon_kb_sum grows 22706.883/h (R2 0.653, 289464.0 -> 367184.0, peak 367552; levels off in the second half)
* listener rss_kb grows 8389.623/h (R2 0.802, 29028.0 -> 44192.0, peak 44228; still growing in the second half (5892.799/h))
* listener anon_kb grows 8647.378/h (R2 0.816, 13908.0 -> 29628.0, peak 29628; still growing in the second half (5924.838/h))
* relay rss_kb grows 1320.282/h (R2 0.651, 12684.0 -> 16324.0, peak 17028; levels off in the second half)
* relay anon_kb grows 1161.368/h (R2 0.734, 5508.0 -> 9100.0, peak 9100; levels off in the second half)
