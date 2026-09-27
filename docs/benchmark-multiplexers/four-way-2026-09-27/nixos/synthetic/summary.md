# Multiplexer benchmark — summary

Run 20260927T081958Z · reps 5 (+1 warm-up discarded)

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
    "tmux": "tmux 3.6a",
    "herdr": "herdr 0.9.1",
    "rmux": "rmux 0.10.0",
    "thurbox": "0.0.0-dev (schema v47)",
    "python": "3.14.7"
  }
}
```

## create

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| N=1 | first_ready_ms | 37.9 | 38.0 | 145 | 147 | 50.9 | 58.3 | 99.6 | 128 |
| N=1 | last_create_ms | 13.2 | 13.3 | 118 | 120 | 16.8 | 27.8 | 99.1 | 123 |
| N=1 | mean_create_ms | 13.2 | 13.3 | 118 | 120 | 16.8 | 27.8 | 99.1 | 123 |
| N=1 | total_ms | 37.9 | 38.0 | 145 | 147 | 50.9 | 58.3 | 99.6 | 128 |
| N=20 | first_ready_ms | 39.4 | 39.7 | 147 | 149 | 41.1 | 41.5 | 65.5 | 66.5 |
| N=20 | last_create_ms | 8.70 | 9.37 | 22.2 | 122 | 10.1 | 12.4 | 35.0 | 37.3 |
| N=20 | mean_create_ms | 8.44 | 8.64 | 35.6 | 59.4 | 9.84 | 10.1 | 35.9 | 36.2 |
| N=20 | total_ms | 197 | 206 | 783 | 1267 | 228 | 230 | 725 | 730 |
| N=5 | first_ready_ms | 39.7 | 41.8 | 151 | 155 | 41.3 | 69.3 | 64.8 | 133 |
| N=5 | last_create_ms | 9.28 | 13.5 | 12.2 | 12.5 | 13.8 | 15.4 | 33.6 | 34.5 |
| N=5 | mean_create_ms | 7.95 | 10.6 | 33.3 | 53.3 | 9.67 | 14.5 | 39.3 | 54.0 |
| N=5 | total_ms | 68.7 | 80.7 | 193 | 309 | 74.7 | 101 | 205 | 277 |
| N=50 | first_ready_ms | 38.9 | 39.9 | 145 | 148 | 41.3 | 41.5 | 62.5 | 78.3 |
| N=50 | last_create_ms | 7.02 | 9.98 | 39.0 | 117 | 9.94 | 11.6 | 35.9 | 48.1 |
| N=50 | mean_create_ms | 8.62 | 8.74 | 54.7 | 56.8 | 10.1 | 10.3 | 35.5 | 38.4 |
| N=50 | total_ms | 458 | 462 | 2776 | 2919 | 532 | 541 | 1782 | 1929 |

## attach

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| N=1 | attach_ms | 11.8 | 12.0 | 243 | 243 | 12.9 | 19.2 | 88.6 | 103 |
| N=1 | detach_ms | 3.32 | 3.40 | 11.1 | 11.1 | 2.77 | 2.81 | 12.3 | 12.8 |
| N=1 | reattach_ms | 11.2 | 11.3 | 243 | 245 | 12.6 | 12.7 | 90.6 | 100.0 |
| N=1 | survivors | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| N=20 | attach_ms | 12.6 | 21.1 | 267 | 271 | 12.8 | 22.6 | 111 | 151 |
| N=20 | detach_ms | 5.17 | 5.25 | 11.1 | 11.1 | 2.66 | 2.74 | 14.9 | 15.3 |
| N=20 | reattach_ms | 12.1 | 12.4 | 280 | 285 | 12.9 | 13.1 | 107 | 169 |
| N=20 | survivors | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 | 20.0 |
| N=50 | attach_ms | 14.3 | 14.5 | 410 | 433 | 13.1 | 13.4 | 425 | 583 |
| N=50 | detach_ms | 7.10 | 8.15 | 20.4 | 20.4 | 2.62 | 2.68 | 22.1 | 23.0 |
| N=50 | reattach_ms | 13.5 | 13.8 | 318 | 418 | 13.3 | 13.4 | 427 | 443 |
| N=50 | survivors | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 | 50.0 |

## resources

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.00 | 1.00 | 0.10 | 0.10 | 2.89 | 2.90 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=1 attached idle | pss_mib | 3.27 | 3.34 | 22.6 | 22.6 | 13.3 | 13.5 | 27.2 | 27.4 |
| N=1 attached idle | rss_mib | 9.17 | 9.25 | 32.7 | 32.7 | 22.4 | 22.8 | 44.3 | 44.5 |
| N=1 attached output | cpu_pct | 0.30 | 0.40 | 8.57 | 8.77 | 0.60 | 0.60 | 8.06 | 8.18 |
| N=1 attached output | pss_mib | 3.27 | 3.34 | 22.6 | 22.6 | 13.3 | 13.5 | 27.4 | 27.6 |
| N=1 attached output | rss_mib | 9.17 | 9.25 | 32.7 | 32.7 | 22.4 | 22.8 | 44.5 | 44.7 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.50 | 0.60 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 1.99 | 2.05 | 17.7 | 17.7 | 9.47 | 9.67 | 6.87 | 6.99 |
| N=1 headless idle | rss_mib | 3.69 | 3.75 | 17.7 | 17.7 | 12.2 | 12.4 | 18.7 | 18.8 |
| N=1 headless output | cpu_pct | 0.20 | 0.20 | 1.00 | 1.10 | 0.20 | 0.30 | 0.20 | 0.30 |
| N=1 headless output | pss_mib | 1.99 | 2.05 | 17.7 | 17.7 | 9.47 | 9.67 | 6.87 | 6.99 |
| N=1 headless output | rss_mib | 3.69 | 3.75 | 17.7 | 17.7 | 12.2 | 12.4 | 18.7 | 18.8 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 20.9 | 27.3 | 0.10 | 0.10 | 5.30 | 5.49 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 attached idle | pss_mib | 5.82 | 5.85 | 34.8 | 34.8 | 14.9 | 15.1 | 33.7 | 34.0 |
| N=20 attached idle | rss_mib | 11.7 | 11.8 | 44.8 | 44.8 | 24.1 | 24.4 | 51.5 | 51.7 |
| N=20 attached output | cpu_pct | 3.39 | 3.69 | 34.1 | 36.4 | 8.19 | 8.20 | 18.2 | 19.7 |
| N=20 attached output | pss_mib | 5.85 | 5.88 | 34.9 | 34.9 | 15.0 | 15.2 | 34.0 | 34.3 |
| N=20 attached output | rss_mib | 11.8 | 11.8 | 44.9 | 44.9 | 24.1 | 24.4 | 51.9 | 52.0 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 9.10 | 9.20 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 2.18 | 2.19 | 26.8 | 26.9 | 10.9 | 10.9 | 7.18 | 7.21 |
| N=20 headless idle | rss_mib | 3.88 | 3.89 | 26.8 | 26.9 | 13.6 | 13.7 | 19.0 | 19.0 |
| N=20 headless idle-long | cpu_pct | 0.00 | 0.00 | 9.15 | 9.35 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 headless idle-long | pss_mib | 2.18 | 2.21 | 26.8 | 26.9 | 10.7 | 10.9 | 7.15 | 7.20 |
| N=20 headless idle-long | rss_mib | 3.88 | 3.91 | 26.9 | 26.9 | 13.4 | 13.7 | 18.9 | 18.9 |
| N=20 headless output | cpu_pct | 3.30 | 3.80 | 19.9 | 27.0 | 2.40 | 2.50 | 2.30 | 3.70 |
| N=20 headless output | pss_mib | 2.32 | 2.32 | 27.2 | 27.2 | 10.9 | 10.9 | 7.28 | 7.31 |
| N=20 headless output | rss_mib | 4.02 | 4.02 | 27.2 | 27.2 | 13.6 | 13.7 | 19.1 | 19.1 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 77.8 | 121 | 0.10 | 0.10 | 9.56 | 9.86 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=50 attached idle | pss_mib | 9.75 | 9.78 | 53.7 | 53.8 | 17.3 | 17.3 | 44.4 | 53.9 |
| N=50 attached idle | rss_mib | 15.7 | 15.8 | 63.7 | 63.8 | 26.3 | 26.5 | 62.3 | 96.2 |
| N=50 attached output | cpu_pct | 6.17 | 6.99 | 92.8 | 113 | 10.9 | 11.0 | 36.6 | 37.7 |
| N=50 attached output | pss_mib | 9.81 | 9.84 | 53.9 | 53.9 | 17.4 | 17.4 | 44.9 | 45.2 |
| N=50 attached output | rss_mib | 15.8 | 15.8 | 63.9 | 63.9 | 26.4 | 26.6 | 62.7 | 63.0 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.3 | 22.4 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 2.44 | 2.46 | 41.1 | 41.1 | 11.5 | 11.8 | 7.54 | 7.58 |
| N=50 headless idle | rss_mib | 4.14 | 4.16 | 41.1 | 41.1 | 14.2 | 14.6 | 19.3 | 19.4 |
| N=50 headless output | cpu_pct | 7.50 | 8.70 | 73.7 | 87.4 | 4.30 | 4.40 | 8.90 | 9.90 |
| N=50 headless output | pss_mib | 2.89 | 2.92 | 41.9 | 41.9 | 11.5 | 11.8 | 7.83 | 7.87 |
| N=50 headless output | rss_mib | 4.59 | 4.62 | 41.9 | 41.9 | 14.2 | 14.6 | 19.6 | 19.7 |

## throughput

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| attached | host_cpu_s | 0.20 | 0.21 | 0.19 | 0.20 | 0.04 | 0.04 | 0.68 | 0.69 |
| attached | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| attached | producer_ms | 207 | 212 | 142 | 147 | 37.6 | 38.9 | 523 | 527 |
| attached | pss_after_mib | 5.48 | 5.50 | 23.2 | 23.2 | 15.1 | 15.2 | 35.6 | 35.7 |
| attached | settle_ms | 334 | 344 | 764 | 1366 | 358 | 359 | 744 | 856 |
| attached | visible_ms | 225 | 225 | 159 | 162 | 218 | 219 | 560 | 564 |
| headless | host_cpu_s | 0.20 | 0.20 | 0.13 | 0.14 | 0.03 | 0.04 | 0.21 | 0.22 |
| headless | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| headless | producer_ms | 200 | 204 | 130 | 130 | 35.6 | 42.8 | 209 | 212 |
| headless | pss_after_mib | 4.17 | 4.19 | 26.4 | 27.2 | 10.5 | 10.6 | 8.62 | 8.65 |
| headless | settle_ms | 326 | 338 | 248 | 252 | 159 | 173 | 334 | 352 |

## scrollback

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| after 1 burst | held_mib | 2.09 | 2.09 | 8.09 | 8.84 | 0.83 | 0.84 | 1.68 | 1.68 |
| after 1 burst | read_all_ms | 6.55 | 12.3 | 8.99 | 9.56 | 6.69 | 11.0 | 46.4 | 52.1 |
| after 1 burst | readable_lines | 2001 | 2001 | 998 | 998 | 2048 | 2048 | 2500 | 2500 |
| after 1 burst | retained_lines | 2001 | 2001 | 5502 | 5502 | 2048 | 2048 | 2500 | 2500 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| idle | echo_ms | 1.05 | 1.10 | 2.21 | 2.34 | 1.30 | 1.37 | 4.17 | 5.14 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.68 | 0.88 | 0.46 | 0.55 | 2.04 | 11.1 | 2.18 | 3.15 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |

## survival

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | rmux median | rmux p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|---|---|
| crash | alive_after_client_kill | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 | 3.00 |
| crash | alive_after_server_kill | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | commands_back | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 3.00 | 3.00 |
| restart | forced_shutdown | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| restart | listed | 0.00 | 0.00 | 3.00 | 3.00 | 0.00 | 0.00 | 3.00 | 3.00 |
| restart | restore_ms | — | — | — | — | — | — | 167 | 175 |
