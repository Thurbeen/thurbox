# Multiplexer benchmark — summary

Run 20260923T154735Z · reps 5 (+1 warm-up discarded)

```json
{
  "machine": {
    "cpus": 4,
    "kernel": "6.12.105+deb13-amd64",
    "ram_gib": 15.5,
    "os": "Debian GNU/Linux 13 (trixie)",
    "cpu_model": "Intel(R) Core(TM) i5-6500T CPU @ 2.50GHz",
    "governor": "powersave"
  },
  "versions": {
    "tmux": "tmux 3.5a",
    "herdr": "herdr 0.9.1",
    "thurbox": "0.0.0-dev (schema v47)",
    "python": "3.13.5"
  }
}
```

## resources

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.29 | 1.29 | 2.99 | 3.10 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=1 attached idle | pss_mib | 10.7 | 10.8 | 28.3 | 29.7 | 28.8 | 29.1 |
| N=1 attached idle | rss_mib | 18.7 | 18.7 | 38.4 | 39.8 | 43.6 | 43.9 |
| N=1 attached output | cpu_pct | 0.30 | 0.40 | 8.39 | 8.66 | 8.18 | 8.39 |
| N=1 attached output | pss_mib | 10.7 | 10.8 | 28.3 | 29.7 | 28.8 | 28.9 |
| N=1 attached output | rss_mib | 18.7 | 18.7 | 38.4 | 39.8 | 43.6 | 43.7 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.60 | 0.70 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 5.18 | 5.21 | 19.7 | 19.7 | 5.90 | 5.93 |
| N=1 headless idle | rss_mib | 8.28 | 8.31 | 19.7 | 19.7 | 13.6 | 13.8 |
| N=1 headless output | cpu_pct | 0.20 | 0.30 | 1.00 | 1.10 | 0.30 | 0.40 |
| N=1 headless output | pss_mib | 5.18 | 5.23 | 19.7 | 23.3 | 5.90 | 5.93 |
| N=1 headless output | rss_mib | 8.28 | 8.32 | 19.7 | 23.4 | 13.6 | 13.8 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 18.2 | 21.8 | 5.99 | 6.39 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=20 attached idle | pss_mib | 13.2 | 13.2 | 38.7 | 52.5 | 49.0 | 49.1 |
| N=20 attached idle | rss_mib | 21.2 | 21.2 | 48.7 | 62.5 | 63.9 | 64.0 |
| N=20 attached output | cpu_pct | 3.88 | 4.10 | 30.3 | 35.4 | 20.5 | 21.5 |
| N=20 attached output | pss_mib | 13.2 | 13.2 | 38.7 | 60.2 | 49.3 | 51.2 |
| N=20 attached output | rss_mib | 21.2 | 21.2 | 48.8 | 70.2 | 64.2 | 66.1 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 8.90 | 9.00 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 5.14 | 5.18 | 28.6 | 28.6 | 5.78 | 5.81 |
| N=20 headless idle | rss_mib | 8.21 | 8.28 | 28.6 | 28.6 | 13.7 | 13.8 |
| N=20 headless output | cpu_pct | 4.10 | 4.60 | 23.2 | 26.6 | 3.90 | 4.90 |
| N=20 headless output | pss_mib | 5.16 | 5.21 | 28.9 | 33.8 | 5.79 | 5.82 |
| N=20 headless output | rss_mib | 8.27 | 8.31 | 28.9 | 33.8 | 13.7 | 13.8 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 71.8 | 96.0 | 11.2 | 13.6 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | 1.00 |
| N=50 attached idle | pss_mib | 16.0 | 16.1 | 57.4 | 81.1 | 104 | 221 |
| N=50 attached idle | rss_mib | 24.0 | 24.1 | 67.5 | 91.1 | 149 | 406 |
| N=50 attached output | cpu_pct | 8.48 | 9.76 | 84.2 | 102 | 39.7 | 41.2 |
| N=50 attached output | pss_mib | 16.2 | 16.2 | 57.6 | 88.4 | 80.3 | 128 |
| N=50 attached output | rss_mib | 24.1 | 24.2 | 67.6 | 98.4 | 95.0 | 209 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.4 | 22.5 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 5.15 | 5.16 | 42.5 | 42.5 | 5.75 | 5.76 |
| N=50 headless idle | rss_mib | 8.20 | 8.30 | 42.5 | 42.5 | 13.6 | 13.7 |
| N=50 headless output | cpu_pct | 11.5 | 12.0 | 54.9 | 76.1 | 10.5 | 13.2 |
| N=50 headless output | pss_mib | 5.17 | 5.18 | 43.4 | 50.2 | 5.76 | 5.77 |
| N=50 headless output | rss_mib | 8.21 | 8.32 | 43.4 | 50.2 | 13.6 | 13.7 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 0.92 | 1.04 | 1.99 | 2.26 | 23.9 | 48.0 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.82 | 1.04 | 0.47 | 0.63 | 42.2 | 43.0 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
