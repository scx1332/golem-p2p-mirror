# Soak run churn-relay

Scenario `churn`, load 1.669 h, warm-up 900.0 s; clients 60, workers 4, interval 1s, relay only True.

| process | metric | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 | growing |
|---|---|---|---|---|---|---|---|---|---|
| cgroup-scope | mem_kb | 26602.0 | 62034.0 | 102128 | 100054.0 | 77676.0 | 26587.305 | 0.976 | **yes** |
| cgroup-slice | mem_kb | 1426504.0 | 1673722.0 | 2972988 | 2018518.0 | 1999150.0 | 295371.344 | 0.114 |  |
| churn* | rss_kb_sum | None | 79884.0 | 80432 | 79838.0 | None | 991.017 | 0.004 |  |
| churn* | anon_kb_sum | None | 17546.0 | 17688 | 17582.0 | None | 206.79 | 0.004 |  |
| churn* | rss_kb_max | None | 20088.0 | 20268 | 20074.0 | None | 58.536 | 0.016 |  |
| churn* | fds_sum | None | 48.0 | 48 | 48.0 | None | 0.732 | 0.006 |  |
| churn* | threads_sum | None | 16.0 | 17 | 16.0 | None | 0.14 | 0.002 |  |
| churn* | n | None | 4.0 | 4 | 4.0 | None | 0.011 | 0.0 |  |
| directory | rss_kb | 9620.0 | 10998.0 | 14056 | 14056.0 | 14056.0 | 2358.509 | 0.903 | **yes** |
| directory | anon_kb | 1268.0 | 2566.0 | 5752 | 5752.0 | 5752.0 | 2447.118 | 0.902 | **yes** |
| directory | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| directory | fds | 11.0 | 14.0 | 15 | 15.0 | 10.0 | 0.202 | 0.009 |  |
| directory | sockets | 5.0 | 8.0 | 9 | 9.0 | 4.0 | 0.202 | 0.009 |  |
| listener | rss_kb | 19942.0 | 26980.0 | 37392 | 37392.0 | 37448.0 | 5787.664 | 0.918 | **yes** |
| listener | anon_kb | 4778.0 | 11248.0 | 22160 | 22160.0 | 22216.0 | 6070.431 | 0.919 | **yes** |
| listener | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| listener | fds | 13.0 | 12.0 | 13 | 13.0 | 12.0 | 0.01 | 0.0 |  |
| listener | sockets | 6.0 | 5.0 | 6 | 6.0 | 5.0 | 0.01 | 0.0 |  |
| relay | rss_kb | 9832.0 | 11208.0 | 12908 | 12908.0 | 12908.0 | 1031.921 | 0.864 |  |
| relay | anon_kb | 1054.0 | 2492.0 | 4176 | 4176.0 | 4176.0 | 976.095 | 0.868 |  |
| relay | threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |  |
| relay | fds | 14.0 | 18.0 | 18 | 18.0 | 14.0 | 0.183 | 0.01 |  |
| relay | sockets | 8.0 | 12.0 | 12 | 12.0 | 8.0 | 0.183 | 0.01 |  |

Listener internals:

| series | idle | after warm-up | peak | load end | after cool-down | slope/h | R2 |
|---|---|---|---|---|---|---|---|
| tokio.alive_tasks | 14.0 | 111.5 | 119 | 111.0 | 13.0 | 0.268 | 0.0 |
| ep.connections | 0.0 | 4.0 | 4 | 4.0 | 0.0 | 0.033 | 0.0 |
| ep.open_connections | 0.0 | 4.0 | 4 | 4.0 | 0.0 | 0.033 | 0.0 |
| ep.connect_slots | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.dial_locks | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.handshakes | 0.0 | 0.0 | 1 | 0.0 | 0.0 | 0.017 | 0.009 |
| ep.queued_streams | 0.0 | 0.0 | 0 | 0.0 | 0.0 | 0.0 | 1.0 |
| ep.cached_identities | 0.0 | 602.0 | 3652 | 3625.0 | 3657.0 | 2185.249 | 1.0 |
| ep.cached_endpoints | 0.0 | 1188.0 | 4095 | 4077.0 | 4060.0 | 2043.925 | 0.778 |
| proc.rss_kb | 20004.0 | 26944.0 | 37392 | 37392.0 | 37448.0 | 5837.947 | 0.917 |
| proc.anon_kb | 4840.0 | 11284.0 | 22160 | 22160.0 | 22216.0 | 6115.302 | 0.918 |
| proc.heap_used_kb | 974.5 | 5446.5 | 11971 | 11948.5 | 10613.0 | 3639.015 | 0.917 |
| proc.heap_free_kb | 3147.0 | 4542.5 | 8985 | 8954.5 | 10346.0 | 2465.32 | 0.9 |
| proc.fds | 13.5 | 13.0 | 14 | 13.5 | 13.0 | 0.009 | 0.0 |
| proc.threads | 7.0 | 7.0 | 7 | 7.0 | 7.0 | 0.0 | 1.0 |

ep.connections_incoming: {'at_load_end': 7299, 'last': 7299}

ep.streams_accepted: {'at_load_end': 109521, 'last': 109521}

ep.handshakes_failed: {'at_load_end': 0, 'last': 0}

ep.streams_refused: {'at_load_end': 0, 'last': 0}

rss_kb_per_connection: {'slope': 1.33577, 'r2': 0.917, 'x_range': [1132, 7288]}

anon_kb_per_connection: {'slope': 1.39924, 'r2': 0.918, 'x_range': [1132, 7288]}

heap_used_kb_per_connection: {'slope': 0.83265, 'r2': 0.917, 'x_range': [1132, 7288]}

Churn: {"sessions": 7299, "exit_codes": {"0": 7299}, "errors": {}, "sessions_per_min": 72.9, "requests": 109415, "requests_failed": 0, "bind_ms": {"p50": 275.671, "p99": 321.961}, "first_ok_ms": {"p50": 382.602, "p99": 493.669}, "bind_ms_trend_per_h": 2.157, "bind_ms_trend_r2": 0.063, "first_ok_ms_trend_per_h": 0.538, "first_ok_ms_trend_r2": 0.001}

Flags:

* directory rss_kb grows 2358.509/h (R2 0.903, 10998.0 -> 14056.0, peak 14056; levels off in the second half)
* directory anon_kb grows 2447.118/h (R2 0.902, 2566.0 -> 5752.0, peak 5752; levels off in the second half)
* listener rss_kb grows 5787.664/h (R2 0.918, 26980.0 -> 37392.0, peak 37392; still growing in the second half (3240.804/h))
* listener anon_kb grows 6070.431/h (R2 0.919, 11248.0 -> 22160.0, peak 22160; still growing in the second half (3261.579/h))
* listener proc.heap_used_kb grows 3639.015/h (R2 0.917, 5446.5 -> 11948.5, peak 11971; still growing in the second half (2221.173/h))
* directory proc.heap_used_kb grows 1963.61/h (R2 0.873, 1244.0 -> 3811.0, peak 3835; levels off in the second half)
* listener errors: {'connection': 41}
