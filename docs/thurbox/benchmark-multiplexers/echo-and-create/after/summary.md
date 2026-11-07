# Multiplexer benchmark — summary

Run 20260923T230517Z · reps 5 (+1 warm-up discarded)

```json
{
  "machine": {
    "cpus": 4,
    "kernel": "6.18.48",
    "ram_gib": 15.5,
    "os": "NixOS 26.05 (Yarara)",
    "cpu_model": "Intel(R) Core(TM) i5-6500T CPU @ 2.50GHz",
    "governor": "powersave"
  },
  "versions": {
    "tmux": "tmux 3.7c",
    "herdr": "herdr 0.9.1",
    "thurbox": "0.0.0-dev (schema v47)",
    "python": "3.13.15"
  }
}
```

## create

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 | first_ready_ms | 37.8 | 38.0 | 149 | 149 | 79.8 | 87.8 |
| N=1 | last_create_ms | 13.5 | 13.5 | 122 | 123 | 78.9 | 87.2 |
| N=1 | mean_create_ms | 13.5 | 13.5 | 122 | 123 | 78.9 | 87.2 |
| N=1 | total_ms | 37.8 | 38.0 | 149 | 149 | 79.8 | 87.8 |
| N=20 | first_ready_ms | 39.1 | 45.5 | 151 | 154 | 69.0 | 104 |
| N=20 | last_create_ms | 6.36 | 8.85 | 21.5 | 128 | 34.9 | 35.6 |
| N=20 | mean_create_ms | 8.43 | 8.63 | 51.4 | 56.9 | 36.2 | 38.5 |
| N=20 | total_ms | 200 | 203 | 1091 | 1179 | 730 | 776 |
| N=5 | first_ready_ms | 39.1 | 39.5 | 151 | 154 | 86.0 | 121 |
| N=5 | last_create_ms | 10.2 | 12.7 | 19.6 | 112 | 34.1 | 43.2 |
| N=5 | mean_create_ms | 8.12 | 8.53 | 52.6 | 54.8 | 45.1 | 59.3 |
| N=5 | total_ms | 67.0 | 73.2 | 308 | 318 | 231 | 294 |
| N=50 | first_ready_ms | 40.4 | 41.5 | 148 | 149 | 63.6 | 95.7 |
| N=50 | last_create_ms | 7.25 | 10.3 | 31.5 | 116 | 36.3 | 36.8 |
| N=50 | mean_create_ms | 8.54 | 8.66 | 51.7 | 54.9 | 36.0 | 36.8 |
| N=50 | total_ms | 454 | 467 | 2658 | 2802 | 1806 | 1844 |

## attach

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 | attach_ms | 10.7 | 10.7 | 243 | 243 | 87.8 | 92.1 |
| N=1 | detach_ms | 2.56 | 2.64 | 11.1 | 11.1 | 12.2 | 12.3 |
| N=1 | reattach_ms | 10.5 | 10.8 | 243 | 245 | 87.7 | 105 |
| N=1 | survivors | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| N=20 | attach_ms | 12.0 | 18.0 | 269 | 288 | 113 | 147 |
| N=20 | detach_ms | 4.95 | 5.01 | 10.8 | 11.1 | 14.9 | 15.3 |
| N=20 | reattach_ms | 11.6 | 11.7 | 264 | 266 | 107 | 112 |
| N=20 | survivors | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 |
| N=50 | attach_ms | 13.3 | 13.8 | 433 | 440 | 178 | 179 |
| N=50 | detach_ms | 7.07 | 7.15 | 10.7 | 20.2 | 21.8 | 24.4 |
| N=50 | reattach_ms | 13.2 | 21.6 | 323 | 423 | 152 | 184 |
| N=50 | survivors | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 |

## resources

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.10 | 1.10 | 2.89 | 2.90 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=1 attached idle | pss_mib | 5.60 | 5.60 | 22.6 | 22.6 | 28.2 | 28.2 |
| N=1 attached idle | rss_mib | 9.57 | 9.57 | 32.7 | 32.7 | 42.5 | 42.5 |
| N=1 attached output | cpu_pct | 0.30 | 0.30 | 8.48 | 8.77 | 7.98 | 8.10 |
| N=1 attached output | pss_mib | 5.60 | 5.60 | 22.6 | 22.6 | 28.7 | 28.7 |
| N=1 attached output | rss_mib | 9.57 | 9.57 | 32.7 | 32.7 | 42.9 | 42.9 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.50 | 0.60 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 3.11 | 3.11 | 17.7 | 17.7 | 8.03 | 8.03 |
| N=1 headless idle | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=1 headless output | cpu_pct | 0.10 | 0.20 | 1.10 | 1.20 | 0.10 | 0.20 |
| N=1 headless output | pss_mib | 3.11 | 3.11 | 17.7 | 17.7 | 8.03 | 8.03 |
| N=1 headless output | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 21.2 | 23.0 | 5.48 | 5.50 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 attached idle | pss_mib | 8.06 | 8.07 | 34.8 | 34.8 | 35.8 | 35.8 |
| N=20 attached idle | rss_mib | 12.0 | 12.0 | 44.8 | 44.8 | 50.1 | 50.1 |
| N=20 attached output | cpu_pct | 3.49 | 4.09 | 34.3 | 41.6 | 18.6 | 19.1 |
| N=20 attached output | pss_mib | 8.09 | 8.10 | 34.9 | 34.9 | 36.2 | 36.3 |
| N=20 attached output | rss_mib | 12.1 | 12.1 | 44.9 | 44.9 | 50.5 | 50.6 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 9.00 | 9.20 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 3.26 | 3.26 | 26.8 | 26.8 | 8.37 | 8.37 |
| N=20 headless idle | rss_mib | 4.00 | 4.00 | 26.8 | 26.8 | 17.4 | 17.4 |
| N=20 headless idle-long | cpu_pct | 0.00 | 0.00 | 9.02 | 11.3 | 0.00 | 0.00 |
| N=20 headless idle-long | pss_mib | 3.26 | 3.26 | 26.8 | 26.8 | 8.37 | 8.37 |
| N=20 headless idle-long | rss_mib | 4.00 | 4.00 | 26.8 | 26.8 | 17.4 | 17.4 |
| N=20 headless output | cpu_pct | 3.30 | 3.90 | 19.8 | 23.1 | 3.50 | 4.00 |
| N=20 headless output | pss_mib | 3.39 | 3.39 | 27.2 | 27.2 | 8.44 | 8.45 |
| N=20 headless output | rss_mib | 4.12 | 4.13 | 27.2 | 27.2 | 17.5 | 17.5 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 86.7 | 119 | 9.67 | 10.1 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=50 attached idle | pss_mib | 12.1 | 12.1 | 53.8 | 53.8 | 52.3 | 55.6 |
| N=50 attached idle | rss_mib | 16.0 | 16.0 | 63.8 | 63.8 | 78.7 | 88.0 |
| N=50 attached output | cpu_pct | 5.87 | 6.99 | 104 | 119 | 36.0 | 36.6 |
| N=50 attached output | pss_mib | 12.1 | 12.1 | 53.9 | 54.0 | 46.7 | 46.9 |
| N=50 attached output | rss_mib | 16.1 | 16.1 | 63.9 | 64.0 | 61.1 | 61.2 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.5 | 27.5 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 3.55 | 3.55 | 41.1 | 41.1 | 8.84 | 8.85 |
| N=50 headless idle | rss_mib | 4.29 | 4.29 | 41.1 | 41.1 | 17.9 | 17.9 |
| N=50 headless output | cpu_pct | 7.40 | 8.00 | 93.1 | 103 | 8.70 | 9.70 |
| N=50 headless output | pss_mib | 3.99 | 4.00 | 41.9 | 41.9 | 9.10 | 9.11 |
| N=50 headless output | rss_mib | 4.73 | 4.74 | 41.9 | 41.9 | 18.2 | 18.2 |

## throughput

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| attached | host_cpu_s | 0.22 | 0.24 | 0.19 | 0.19 | 0.37 | 0.39 |
| attached | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| attached | producer_ms | 228 | 229 | 142 | 156 | 246 | 250 |
| attached | pss_after_mib | 7.72 | 7.73 | 23.2 | 26.6 | 36.8 | 36.8 |
| attached | settle_ms | 446 | 461 | 776 | 778 | 371 | 490 |
| attached | visible_ms | 334 | 335 | 158 | 163 | 260 | 262 |
| headless | host_cpu_s | 0.22 | 0.22 | 0.13 | 0.14 | 0.22 | 0.24 |
| headless | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| headless | producer_ms | 222 | 226 | 130 | 134 | 227 | 229 |
| headless | pss_after_mib | 5.21 | 5.21 | 24.9 | 25.3 | 9.73 | 9.73 |
| headless | settle_ms | 354 | 361 | 257 | 276 | 361 | 367 |

## scrollback

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| after 1 burst | held_mib | 2.08 | 2.08 | 6.95 | 9.20 | 1.67 | 1.67 |
| after 1 burst | read_all_ms | 7.80 | 9.45 | 7.47 | 7.89 | 41.9 | 46.4 |
| after 1 burst | readable_lines | 2001 | 2001 | 998 | 998 | 2500 | 2500 |
| after 1 burst | retained_lines | 2001 | 2001 | 5502 | 5502 | 2500 | 2500 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 0.97 | 1.05 | 2.06 | 2.20 | 3.37 | 4.66 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.74 | 0.98 | 0.45 | 0.57 | 1.65 | 2.48 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |

## survival

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| crash | alive_after_client_kill | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| crash | alive_after_server_kill | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | commands_back | 0.00 | 0.00 | 0.00 | 0.00 | 3.00 | 3.00 |
| restart | forced_shutdown | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | listed | 0.00 | 0.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| restart | restore_ms | — | — | — | — | 167 | 172 |
