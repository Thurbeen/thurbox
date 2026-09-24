# Multiplexer benchmark — summary

Run 20260923T213014Z · reps 5 (+1 warm-up discarded)

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
| N=1 | first_ready_ms | 37.8 | 37.9 | 148 | 148 | 94.0 | 104 |
| N=1 | last_create_ms | 13.5 | 13.6 | 122 | 123 | 121 | 135 |
| N=1 | mean_create_ms | 13.5 | 13.6 | 122 | 123 | 121 | 135 |
| N=1 | total_ms | 37.8 | 37.9 | 148 | 148 | 94.0 | 104 |
| N=20 | first_ready_ms | 39.3 | 40.9 | 152 | 153 | 110 | 151 |
| N=20 | last_create_ms | 6.52 | 13.4 | 32.9 | 126 | 88.6 | 89.5 |
| N=20 | mean_create_ms | 8.39 | 8.56 | 41.3 | 49.5 | 98.6 | 133 |
| N=20 | total_ms | 194 | 198 | 882 | 1043 | 1948 | 2627 |
| N=5 | first_ready_ms | 39.1 | 55.0 | 151 | 152 | 137 | 148 |
| N=5 | last_create_ms | 11.6 | 14.8 | 15.4 | 20.2 | 86.8 | 88.0 |
| N=5 | mean_create_ms | 8.27 | 11.6 | 53.0 | 55.4 | 104 | 115 |
| N=5 | total_ms | 66.3 | 81.2 | 296 | 315 | 500 | 551 |
| N=50 | first_ready_ms | 40.4 | 42.0 | 151 | 155 | 107 | 141 |
| N=50 | last_create_ms | 9.43 | 13.5 | 35.7 | 128 | 91.2 | 92.4 |
| N=50 | mean_create_ms | 8.55 | 8.73 | 56.4 | 59.2 | 95.3 | 101 |
| N=50 | total_ms | 456 | 461 | 2885 | 3034 | 4739 | 5024 |

## attach

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 | attach_ms | 10.7 | 10.8 | 243 | 243 | 162 | 191 |
| N=1 | detach_ms | 2.57 | 2.63 | 10.8 | 11.1 | 12.3 | 12.5 |
| N=1 | reattach_ms | 10.4 | 11.8 | 243 | 244 | 180 | 194 |
| N=1 | survivors | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| N=20 | attach_ms | 11.8 | 11.9 | 267 | 272 | 169 | 214 |
| N=20 | detach_ms | 4.99 | 5.05 | 11.0 | 11.1 | 15.1 | 15.3 |
| N=20 | reattach_ms | 11.7 | 17.9 | 278 | 286 | 192 | 205 |
| N=20 | survivors | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 |
| N=50 | attach_ms | 13.2 | 19.8 | 417 | 446 | 210 | 464 |
| N=50 | detach_ms | 7.09 | 7.22 | 10.5 | 18.4 | 21.9 | 23.1 |
| N=50 | reattach_ms | 13.5 | 13.5 | 420 | 454 | 249 | 494 |
| N=50 | survivors | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 |

## resources

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.10 | 1.10 | 2.90 | 3.09 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=1 attached idle | pss_mib | 5.60 | 5.60 | 22.6 | 22.6 | 27.9 | 27.9 |
| N=1 attached idle | rss_mib | 9.57 | 9.57 | 32.7 | 32.7 | 42.4 | 42.4 |
| N=1 attached output | cpu_pct | 0.30 | 0.40 | 8.48 | 8.67 | 7.88 | 8.09 |
| N=1 attached output | pss_mib | 5.60 | 5.60 | 22.6 | 22.6 | 28.4 | 28.4 |
| N=1 attached output | rss_mib | 9.57 | 9.57 | 32.7 | 32.7 | 42.8 | 42.8 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.60 | 0.60 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 3.11 | 3.11 | 17.7 | 17.7 | 7.89 | 7.89 |
| N=1 headless idle | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=1 headless output | cpu_pct | 0.20 | 0.20 | 1.00 | 1.10 | 0.20 | 0.20 |
| N=1 headless output | pss_mib | 3.11 | 3.11 | 17.7 | 17.7 | 7.89 | 7.89 |
| N=1 headless output | rss_mib | 3.84 | 3.84 | 17.7 | 17.7 | 17.1 | 17.1 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 17.5 | 23.5 | 5.49 | 5.58 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 attached idle | pss_mib | 8.06 | 8.06 | 34.8 | 34.8 | 35.5 | 35.9 |
| N=20 attached idle | rss_mib | 12.0 | 12.0 | 44.8 | 44.8 | 49.9 | 50.3 |
| N=20 attached output | cpu_pct | 3.78 | 3.89 | 31.2 | 36.1 | 19.5 | 19.7 |
| N=20 attached output | pss_mib | 8.08 | 8.10 | 34.9 | 34.9 | 36.0 | 36.5 |
| N=20 attached output | rss_mib | 12.1 | 12.1 | 44.9 | 44.9 | 50.4 | 51.0 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 8.90 | 9.10 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 3.26 | 3.26 | 26.8 | 26.8 | 8.23 | 8.23 |
| N=20 headless idle | rss_mib | 4.00 | 4.00 | 26.8 | 26.8 | 17.4 | 17.4 |
| N=20 headless idle-long | cpu_pct | 0.00 | 0.00 | 9.06 | 9.17 | 0.00 | 0.00 |
| N=20 headless idle-long | pss_mib | 3.26 | 3.26 | 26.8 | 26.8 | 8.23 | 8.23 |
| N=20 headless idle-long | rss_mib | 4.00 | 4.00 | 26.8 | 26.9 | 17.4 | 17.4 |
| N=20 headless output | cpu_pct | 3.60 | 3.90 | 21.4 | 25.0 | 4.10 | 4.80 |
| N=20 headless output | pss_mib | 3.39 | 3.39 | 27.2 | 27.2 | 8.31 | 8.31 |
| N=20 headless output | rss_mib | 4.12 | 4.13 | 27.2 | 27.2 | 17.5 | 17.5 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 88.4 | 91.4 | 9.89 | 10.1 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=50 attached idle | pss_mib | 12.1 | 12.1 | 53.7 | 53.8 | 46.0 | 46.1 |
| N=50 attached idle | rss_mib | 16.0 | 16.0 | 63.7 | 63.8 | 60.5 | 60.6 |
| N=50 attached output | cpu_pct | 6.29 | 6.68 | 86.3 | 112 | 35.9 | 37.4 |
| N=50 attached output | pss_mib | 12.1 | 12.1 | 53.9 | 53.9 | 46.6 | 46.7 |
| N=50 attached output | rss_mib | 16.1 | 16.1 | 63.9 | 63.9 | 61.0 | 61.1 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.3 | 22.9 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 3.54 | 3.54 | 41.1 | 41.1 | 8.70 | 8.70 |
| N=50 headless idle | rss_mib | 4.28 | 4.28 | 41.1 | 41.1 | 17.9 | 17.9 |
| N=50 headless output | cpu_pct | 7.50 | 8.00 | 74.4 | 110 | 8.00 | 9.10 |
| N=50 headless output | pss_mib | 3.99 | 4.01 | 41.9 | 41.9 | 8.97 | 8.97 |
| N=50 headless output | rss_mib | 4.73 | 4.75 | 41.9 | 41.9 | 18.2 | 18.2 |

## throughput

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| attached | host_cpu_s | 0.23 | 0.24 | 0.19 | 0.21 | 0.39 | 0.40 |
| attached | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| attached | producer_ms | 228 | 228 | 143 | 150 | 247 | 255 |
| attached | pss_after_mib | 7.70 | 7.72 | 23.2 | 26.2 | 36.0 | 36.0 |
| attached | settle_ms | 446 | 463 | 532 | 1034 | 575 | 616 |
| attached | visible_ms | 334 | 338 | 157 | 161 | 259 | 260 |
| headless | host_cpu_s | 0.21 | 0.22 | 0.13 | 0.15 | 0.22 | 0.22 |
| headless | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| headless | producer_ms | 207 | 226 | 130 | 131 | 214 | 226 |
| headless | pss_after_mib | 5.21 | 5.21 | 24.9 | 27.2 | 9.60 | 9.60 |
| headless | settle_ms | 346 | 366 | 257 | 745 | 341 | 362 |

## scrollback

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| after 1 burst | held_mib | 2.08 | 2.08 | 6.96 | 8.08 | 1.67 | 1.67 |
| after 1 burst | read_all_ms | 8.97 | 10.00 | 6.22 | 6.29 | 42.0 | 52.5 |
| after 1 burst | readable_lines | 2001 | 2001 | 998 | 998 | 2500 | 2500 |
| after 1 burst | retained_lines | 2001 | 2001 | 5502 | 5502 | 2500 | 2500 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 1.00 | 1.08 | 2.13 | 2.33 | 24.7 | 48.3 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.76 | 0.94 | 0.46 | 0.57 | 42.1 | 43.2 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |

## survival

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| crash | alive_after_client_kill | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| crash | alive_after_server_kill | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | commands_back | 0.00 | 0.00 | 0.00 | 0.00 | 3.00 | 3.00 |
| restart | forced_shutdown | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | listed | 0.00 | 0.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| restart | restore_ms | — | — | — | — | 231 | 242 |
