# Soak run churn

Scenario `churn`, load 2.503 h, warm-up 900.0 s; clients 60, workers 6, interval 1s, relay only False.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 25502.0 | 80864.0 | 151812 | 150204 | 114952.0 | 29254.006 | 0.987 | **yes** |
| cgroup-slice | mem_kb | 132512.0 | 1199592.0 | 2972028 | 1985620 | 401336.0 | 159431.605 | 0.045 |  |
| churn* | rss_kb_sum | None | 121240.0 | 122172 | 121068 | None | -11.592 | 0.0 |  |
| churn* | anon_kb_sum | None | 27656.0 | 27856 | 27716 | None | 18.828 | 0.0 |  |
| churn* | rss_kb_max | None | 20354.0 | 20480 | 20340 | None | -9.851 | 0.009 |  |
| churn* | fds_sum | None | 78.0 | 78 | 78 | None | 0.027 | 0.0 |  |
| churn* | threads_sum | None | 24.0 | 29 | 24 | None | -0.122 | 0.004 |  |
| churn* | n | None | 6.0 | 6 | 6 | None | 0.005 | 0.0 |  |
| directory | rss_kb | 9708.0 | 11630.0 | 17128 | 17128 | 17128.0 | 2439.654 | 0.849 | **yes** |
| directory | anon_kb | 1268.0 | 3138.0 | 8876 | 8876 | 8876.0 | 2523.156 | 0.85 | **yes** |
| directory | threads | 7.0 | 7.0 | 7 | 7 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 16.0 | 17 | 16 | 10.0 | -0.044 | 0.001 |  |
| directory | sockets | 5.0 | 10.0 | 11 | 10 | 4.0 | -0.044 | 0.001 |  |
| listener | rss_kb | 20440.0 | 28924.0 | 48776 | 48776 | 48776.0 | 7544.469 | 0.929 | **yes** |
| listener | anon_kb | 4980.0 | 13844.0 | 33452 | 33452 | 33452.0 | 7572.591 | 0.928 | **yes** |
| listener | threads | 7.0 | 7.0 | 8 | 7 | 7.0 | -0.002 | 0.0 |  |
| listener | fds | 14.0 | 13.0 | 14 | 13 | 13.0 | -0.012 | 0.001 |  |
| listener | sockets | 7.0 | 6.0 | 7 | 6 | 6.0 | -0.012 | 0.001 |  |
| relay | rss_kb | 10580.0 | 14342.0 | 18272 | 17872 | 17872.0 | 1601.615 | 0.878 | **yes** |
| relay | anon_kb | 1232.0 | 6598.0 | 9988 | 9988 | 9988.0 | 1369.083 | 0.96 | **yes** |
| relay | threads | 7.0 | 7.0 | 7 | 7 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 20.0 | 20 | 20 | 14.0 | 0.002 | 0.0 |  |
| relay | sockets | 8.0 | 14.0 | 14 | 14 | 8.0 | 0.002 | 0.0 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 16.0 | 158.5 | 168 | 160.0 | 15.0 | 0.383 | 0.001 |
| ep.connections | 0.0 | 5.5 | 6 | 5.5 | 0.0 | 0.014 | 0.0 |
| ep.open_connections | 0.0 | 5.5 | 6 | 5.5 | 0.0 | 0.014 | 0.0 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 1 | 0.0 | 0.0 | -0.004 | 0.001 |
| ep.queued_streams | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.cached_identities | 0.0 | 901.5 | 4096 | 3640.5 | 3690.0 | 1165.138 | 0.677 |
| ep.cached_endpoints | 0.0 | 1779.0 | 4096 | 4079.0 | 4063.0 | 464.808 | 0.33 |
| proc.rss_kb | 20476.0 | 29308.0 | 48776 | 48776.0 | 48776.0 | 7520.844 | 0.929 |
| proc.anon_kb | 5030.0 | 14228.0 | 33452 | 33452.0 | 33452.0 | 7547.751 | 0.928 |
| proc.heap_used_kb | 4076.5 | 10038.0 | 20620 | 19833.0 | 17779.0 | 3713.164 | 0.868 |
| proc.heap_free_kb | 4180.0 | 6882.0 | 16448 | 16254.0 | 18308.0 | 3816.534 | 0.935 |
| proc.fds | 14.5 | 14.0 | 15 | 14.0 | 14.0 | -0.016 | 0.001 |
| proc.threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |

ep.connections_incoming: {'at_load_end': 16322, 'last': 16322}

ep.streams_accepted: {'at_load_end': 244907, 'last': 244907}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

rss_kb_per_connection: {'slope': 1.15595, 'r2': 0.929, 'x_range': [1698, 16304]}

anon_kb_per_connection: {'slope': 1.16009, 'r2': 0.928, 'x_range': [1698, 16304]}

heap_used_kb_per_connection: {'slope': 0.57077, 'r2': 0.868, 'x_range': [1698, 16304]}

Churn: {"sessions": 16322, "exit_codes": {"0": 16322}, "errors": {}, "sessions_per_min": 108.7, "requests": 244744, "requests_failed": 0, "bind_ms": {"p50": 297.242, "p99": 357.971}, "first_ok_ms": {"p50": 403.504, "p99": 520.48}, "bind_ms_trend_per_h": -2.144, "bind_ms_trend_r2": 0.024, "first_ok_ms_trend_per_h": -0.963, "first_ok_ms_trend_r2": 0.003}

Flags:

* directory rss_kb grows 2439.654/h (R2 0.849, 11630.0 -> 17128, peak 17128; levels off in the second half)
* directory anon_kb grows 2523.156/h (R2 0.85, 3138.0 -> 8876, peak 8876; levels off in the second half)
* listener rss_kb grows 7544.469/h (R2 0.929, 28924.0 -> 48776, peak 48776; still growing in the second half (5952.386/h))
* listener anon_kb grows 7572.591/h (R2 0.928, 13844.0 -> 33452, peak 33452; still growing in the second half (6009.066/h))
* relay rss_kb grows 1601.615/h (R2 0.878, 14342.0 -> 17872, peak 18272; levels off in the second half)
* relay anon_kb grows 1369.083/h (R2 0.96, 6598.0 -> 9988, peak 9988; still growing in the second half (1537.616/h))
* listener proc.heap_used_kb grows 3713.164/h (R2 0.868, 10038.0 -> 19833.0, peak 20620; still growing in the second half (3672.659/h))
* directory proc.heap_used_kb grows 1718.048/h (R2 0.692, 1850.5 -> 6159.5, peak 6260; levels off in the second half)
* listener errors: {'connection': 16}
