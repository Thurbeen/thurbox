# Multiplexer benchmark — summary

Run 20260923T085355Z + 20260923T091234Z · reps 5 (+1 warm-up discarded)

```json
{
  "machine": {
    "cpu_model": "Intel(R) Core(TM) i5-6500T CPU @ 2.50GHz",
    "cpus": 4,
    "governor": "powersave",
    "kernel": "6.18.48",
    "os": "NixOS 26.05 (Yarara)",
    "ram_gib": 15.5
  },
  "versions": {
    "herdr": "herdr 0.9.1",
    "python": "3.13.15",
    "thurbox": "0.0.0-dev (schema v47)",
    "tmux": "tmux 3.7c"
  }
}
```

## create

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 | first_ready_ms | 37.8 | 37.9 | 147 | 148 | 88.1 | 92.1 |
| N=1 | last_create_ms | 13.5 | 13.6 | 121 | 122 | 118 | 119 |
| N=1 | mean_create_ms | 13.5 | 13.6 | 121 | 122 | 118 | 119 |
| N=1 | total_ms | 37.8 | 37.9 | 147 | 148 | 88.1 | 92.1 |
| N=20 | first_ready_ms | 39.0 | 39.2 | 154 | 155 | 89.0 | 107 |
| N=20 | last_create_ms | 7.35 | 8.94 | 23.5 | 123 | 89.0 | 99.2 |
| N=20 | mean_create_ms | 8.48 | 8.62 | 47.7 | 54.6 | 96.2 | 101 |
| N=20 | total_ms | 196 | 200 | 1031 | 1152 | 1899 | 1991 |
| N=5 | first_ready_ms | 39.1 | 48.4 | 153 | 155 | 98.8 | 154 |
| N=5 | last_create_ms | 11.3 | 14.8 | 10.4 | 110 | 87.0 | 88.8 |
| N=5 | mean_create_ms | 7.86 | 10.3 | 34.2 | 53.5 | 95.8 | 118 |
| N=5 | total_ms | 64.3 | 73.7 | 197 | 308 | 457 | 569 |
| N=50 | first_ready_ms | 39.0 | 39.3 | 150 | 153 | 89.0 | 115 |
| N=50 | last_create_ms | 7.95 | 13.3 | 27.6 | 117 | 91.7 | 92.2 |
| N=50 | mean_create_ms | 8.61 | 8.73 | 50.7 | 60.9 | 92.1 | 104 |
| N=50 | total_ms | 460 | 470 | 2582 | 3087 | 4579 | 5197 |

## attach

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 | attach_ms | 10.8 | 16.2 | 67.6 | 68.7 | 168 | 179 |
| N=1 | detach_ms | 2.65 | 2.66 | 11.0 | 12.4 | 12.3 | 12.5 |
| N=1 | reattach_ms | 10.1 | 10.8 | 182 | 183 | 177 | 187 |
| N=1 | survivors | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| N=20 | attach_ms | 11.6 | 11.8 | 289 | 306 | 185 | 193 |
| N=20 | detach_ms | 5.02 | 5.70 | 10.9 | 11.6 | 16.1 | 17.2 |
| N=20 | reattach_ms | 8.28 | 11.1 | 170 | 175 | 178 | 206 |
| N=20 | survivors | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 |
| N=50 | attach_ms | 13.3 | 13.6 | 435 | 446 | 242 | 254 |
| N=50 | detach_ms | 6.96 | 7.05 | 10.8 | 21.2 | 23.6 | 26.4 |
| N=50 | reattach_ms | 9.09 | 9.11 | 411 | 430 | 431 | 592 |
| N=50 | survivors | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 |

## resources

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.00 | 1.10 | 2.80 | 2.99 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=1 attached idle | pss_mib | 6.24 | 6.24 | 22.6 | 22.6 | 28.6 | 28.6 |
| N=1 attached idle | rss_mib | 9.59 | 9.59 | 32.7 | 32.7 | 42.8 | 42.8 |
| N=1 attached output | cpu_pct | 0.30 | 0.30 | 8.68 | 8.78 | 8.06 | 8.39 |
| N=1 attached output | pss_mib | 6.24 | 6.24 | 22.6 | 22.6 | 28.6 | 28.7 |
| N=1 attached output | rss_mib | 9.59 | 9.59 | 32.7 | 32.7 | 42.8 | 42.9 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.60 | 0.60 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 3.71 | 3.71 | 17.7 | 17.7 | 8.32 | 8.32 |
| N=1 headless idle | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=1 headless output | cpu_pct | 0.10 | 0.20 | 1.00 | 1.10 | 0.20 | 0.30 |
| N=1 headless output | pss_mib | 3.71 | 3.71 | 17.7 | 17.7 | 8.32 | 8.32 |
| N=1 headless output | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 18.7 | 20.5 | 5.29 | 5.30 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=20 attached idle | pss_mib | 8.71 | 8.71 | 34.8 | 34.9 | 49.1 | 49.2 |
| N=20 attached idle | rss_mib | 12.1 | 12.1 | 44.8 | 44.9 | 63.4 | 63.4 |
| N=20 attached output | cpu_pct | 3.99 | 4.09 | 33.7 | 34.2 | 19.8 | 20.4 |
| N=20 attached output | pss_mib | 8.73 | 8.75 | 34.9 | 34.9 | 49.3 | 49.4 |
| N=20 attached output | rss_mib | 12.1 | 12.1 | 44.9 | 44.9 | 63.6 | 63.6 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 9.00 | 9.10 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 3.87 | 3.87 | 26.8 | 26.9 | 8.67 | 8.67 |
| N=20 headless idle | rss_mib | 4.00 | 4.00 | 26.9 | 26.9 | 17.5 | 17.5 |
| N=20 headless idle-long | cpu_pct | 0.00 | 0.00 | 8.98 | 9.08 | 0.00 | 0.00 |
| N=20 headless idle-long | pss_mib | 3.87 | 3.87 | 26.8 | 26.9 | 8.67 | 8.67 |
| N=20 headless idle-long | rss_mib | 4.00 | 4.00 | 26.9 | 26.9 | 17.5 | 17.5 |
| N=20 headless output | cpu_pct | 3.90 | 4.20 | 21.0 | 22.2 | 4.30 | 4.30 |
| N=20 headless output | pss_mib | 4.00 | 4.00 | 27.2 | 27.2 | 8.74 | 8.74 |
| N=20 headless output | rss_mib | 4.12 | 4.13 | 27.2 | 27.2 | 17.5 | 17.5 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 75.8 | 89.2 | 10.1 | 12.1 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=50 attached idle | pss_mib | 12.7 | 12.7 | 53.8 | 53.8 | 80.5 | 89.4 |
| N=50 attached idle | rss_mib | 16.0 | 16.1 | 63.8 | 63.8 | 94.7 | 122 |
| N=50 attached output | cpu_pct | 6.18 | 7.39 | 89.8 | 106 | 38.7 | 38.9 |
| N=50 attached output | pss_mib | 12.7 | 12.8 | 53.9 | 53.9 | 82.6 | 90.8 |
| N=50 attached output | rss_mib | 16.1 | 16.1 | 63.9 | 63.9 | 99.6 | 129 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.5 | 23.5 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 4.16 | 4.16 | 41.1 | 41.1 | 9.16 | 9.16 |
| N=50 headless idle | rss_mib | 4.29 | 4.29 | 41.1 | 41.1 | 18.0 | 18.0 |
| N=50 headless output | cpu_pct | 7.20 | 7.90 | 70.0 | 87.9 | 8.70 | 8.90 |
| N=50 headless output | pss_mib | 4.60 | 4.61 | 41.9 | 41.9 | 9.42 | 9.43 |
| N=50 headless output | rss_mib | 4.73 | 4.73 | 41.9 | 41.9 | 18.2 | 18.2 |

## throughput

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| attached | host_cpu_s | 0.23 | 0.23 | 0.20 | 0.20 | 0.38 | 0.41 |
| attached | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| attached | producer_ms | 233 | 234 | 143 | 143 | 246 | 248 |
| attached | pss_after_mib | 8.36 | 8.38 | 23.2 | 23.2 | 36.5 | 36.5 |
| attached | settle_ms | 447 | 449 | 790 | 1036 | 442 | 585 |
| attached | visible_ms | 333 | 337 | 158 | 159 | 260 | 263 |
| headless | host_cpu_s | 0.21 | 0.22 | 0.14 | 0.16 | 0.21 | 0.23 |
| headless | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| headless | producer_ms | 215 | 218 | 127 | 131 | 215 | 221 |
| headless | pss_after_mib | 5.85 | 5.85 | 23.4 | 24.9 | 10.0 | 10.0 |
| headless | settle_ms | 343 | 352 | 263 | 1262 | 353 | 355 |

## scrollback

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| after 1 burst | held_mib | 2.11 | 2.11 | 6.96 | 6.96 | 1.66 | 1.66 |
| after 1 burst | read_all_ms | 7.82 | 9.15 | 6.25 | 6.29 | 47.4 | 49.7 |
| after 1 burst | readable_lines | 2001 | 2001 | 998 | 998 | 2500 | 2500 |
| after 1 burst | retained_lines | 2001 | 2001 | 5502 | 5502 | 2500 | 2500 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 1.04 | 1.09 | 2.23 | 2.34 | 24.0 | 48.3 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.77 | 0.98 | 0.45 | 0.55 | 42.2 | 43.1 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |

## survival

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| crash | alive_after_client_kill | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| crash | alive_after_server_kill | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | commands_back | 0.00 | 0.00 | 0.00 | 0.00 | 3.00 | 3.00 |
| restart | listed | 0.00 | 0.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| restart | restore_ms | — | — | — | — | 222 | 270 |
