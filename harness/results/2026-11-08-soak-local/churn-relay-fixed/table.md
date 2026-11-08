# Soak run churn-relay-fixed

Scenario `churn`, load 0.334 h, warm-up 300.22678369283676 s; clients 60, workers 4, interval 1s, relay only True.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 29448.0 | 61076.0 | 71548 | 69468.0 | 47186.0 | 41557.545 | 0.811 | **yes** |
| cgroup-slice | mem_kb | 372754.0 | 187478.0 | 401296 | 192320.0 | 170034.0 | 24199.1 | 0.696 |  |
| churn* | rss_kb_sum | None | 80196.0 | 80572 | 80194.0 | None | 7472.638 | 0.011 |  |
| churn* | anon_kb_sum | None | 17670.0 | 17728 | 17668.0 | None | 1619.767 | 0.012 |  |
| churn* | rss_kb_max | None | 20128.0 | 20236 | 20148.0 | None | 67.611 | 0.002 |  |
| churn* | fds_sum | None | 48.0 | 48 | 48.0 | None | 4.161 | 0.009 |  |
| churn* | threads_sum | None | 16.0 | 16 | 16.0 | None | 1.55 | 0.014 |  |
| churn* | n | None | 4.0 | 4 | 4.0 | None | 0.394 | 0.015 |  |
| directory | rss_kb | 9758.0 | 10660.0 | 11512 | 11476.0 | 11524.0 | 3895.082 | 0.986 |  |
| directory | anon_kb | 1258.0 | 2032.0 | 2884 | 2848.0 | 2896.0 | 3895.082 | 0.986 |  |
| directory | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 14.0 | 15 | 14.5 | 10.0 | 1.808 | 0.023 |  |
| directory | sockets | 5.0 | 8.0 | 9 | 8.5 | 4.0 | 1.808 | 0.023 |  |
| listener | rss_kb | 19886.0 | 25002.0 | 27172 | 27084.0 | 27172.0 | 10027.099 | 0.953 | **yes** |
| listener | anon_kb | 4594.0 | 9570.0 | 11740 | 11652.0 | 11740.0 | 10026.98 | 0.953 | **yes** |
| listener | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| listener | fds | 13.0 | 12.0 | 13 | 13.0 | 12.0 | 1.722 | 0.215 |  |
| listener | sockets | 6.0 | 5.0 | 6 | 6.0 | 5.0 | 1.722 | 0.215 |  |
| relay | rss_kb | 9780.0 | 11438.0 | 11696 | 11692.0 | 11696.0 | 1160.89 | 0.884 |  |
| relay | anon_kb | 996.0 | 2654.0 | 2944 | 2940.0 | 2944.0 | 1331.056 | 0.897 |  |
| relay | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 18.0 | 18 | 18.0 | 14.0 | 0.16 | 0.0 |  |
| relay | sockets | 8.0 | 12.0 | 12 | 12.0 | 8.0 | 0.16 | 0.0 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 15.0 | 109.0 | 115 | 110.5 | 13.0 | 11.159 | 0.021 |
| ep.connections | 0.0 | 4.0 | 4 | 4.0 | 0.0 | 0.961 | 0.007 |
| ep.open_connections | 0.0 | 4.0 | 4 | 4.0 | 0.0 | 0.961 | 0.007 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.queued_streams | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.cached_identities | 0.0 | 237.0 | 740 | 713.0 | 742.0 | 2194.865 | 1.0 |
| ep.cached_endpoints | 0.0 | 458.0 | 1464 | 1410.0 | 1468.0 | 4391.065 | 1.0 |
| proc.rss_kb | 19742.0 | 25020.0 | 27172 | 27122.0 | 27172.0 | 9878.068 | 0.953 |
| proc.anon_kb | 4694.0 | 9588.0 | 11740 | 11690.0 | 11740.0 | 9877.001 | 0.952 |
| proc.heap_used_kb | 963.0 | 4372.5 | 5706 | 5653.5 | 4303.0 | 5896.579 | 0.929 |
| proc.heap_free_kb | 3140.0 | 3972.5 | 4871 | 4808.5 | 6196.0 | 3974.949 | 0.812 |
| proc.fds | 14.0 | 13.0 | 14 | 13.5 | 13.0 | 1.495 | 0.187 |
| proc.threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |

ep.connections_incoming: {'at_load_end': 1468, 'last': 1468}

ep.streams_accepted: {'at_load_end': 22026, 'last': 22026}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

rss_kb_per_connection: {'slope': 2.24946, 'r2': 0.953, 'x_range': [404, 1464]}

anon_kb_per_connection: {'slope': 2.24921, 'r2': 0.952, 'x_range': [404, 1464]}

heap_used_kb_per_connection: {'slope': 1.34311, 'r2': 0.929, 'x_range': [404, 1464]}

Churn: {"sessions": 1468, "exit_codes": {"0": 1468}, "errors": {}, "sessions_per_min": 73.34, "requests": 22026, "requests_failed": 0, "bind_ms": {"p50": 266.983, "p99": 282.114}, "first_ok_ms": {"p50": 371.285, "p99": 478.037}}

Flags:

* listener rss_kb grows 10027.099/h (R2 0.953, 25002.0 -> 27084.0, peak 27172; levels off in the second half)
* listener anon_kb grows 10026.98/h (R2 0.953, 9570.0 -> 11652.0, peak 11740; levels off in the second half)
