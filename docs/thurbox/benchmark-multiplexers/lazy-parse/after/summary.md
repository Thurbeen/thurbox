# Multiplexer benchmark — summary

Run 20260923T163847Z · reps 5 (+1 warm-up discarded)

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
| N=1 attached idle | cpu_pct | 0.00 | 0.00 | 1.09 | 1.29 | 3.09 | 3.19 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=1 attached idle | pss_mib | 11.1 | 11.2 | 26.6 | 27.9 | 29.0 | 29.3 |
| N=1 attached idle | rss_mib | 18.6 | 18.7 | 36.7 | 38.0 | 43.5 | 43.7 |
| N=1 attached output | cpu_pct | 0.30 | 0.30 | 8.59 | 8.69 | 8.27 | 8.37 |
| N=1 attached output | pss_mib | 11.1 | 11.2 | 26.6 | 28.2 | 29.6 | 29.6 |
| N=1 attached output | rss_mib | 18.6 | 18.7 | 36.7 | 38.2 | 43.9 | 44.2 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 | 0.60 | 0.60 | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 5.25 | 5.35 | 19.7 | 19.7 | 5.97 | 6.04 |
| N=1 headless idle | rss_mib | 8.16 | 8.30 | 19.7 | 19.7 | 13.6 | 13.9 |
| N=1 headless output | cpu_pct | 0.10 | 0.20 | 1.00 | 1.00 | 0.20 | 0.30 |
| N=1 headless output | pss_mib | 5.25 | 5.35 | 19.7 | 19.7 | 5.97 | 6.04 |
| N=1 headless output | rss_mib | 8.16 | 8.30 | 19.7 | 19.7 | 13.6 | 13.9 |
| N=20 attached idle | cpu_pct | 0.00 | 0.00 | 17.3 | 23.9 | 6.19 | 6.29 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=20 attached idle | pss_mib | 13.6 | 13.6 | 40.7 | 59.5 | 36.5 | 36.8 |
| N=20 attached idle | rss_mib | 21.1 | 21.3 | 50.7 | 69.5 | 50.9 | 51.3 |
| N=20 attached output | cpu_pct | 3.40 | 4.60 | 32.1 | 36.5 | 20.8 | 21.2 |
| N=20 attached output | pss_mib | 13.6 | 13.6 | 46.7 | 59.5 | 36.5 | 36.7 |
| N=20 attached output | rss_mib | 21.1 | 21.3 | 56.7 | 69.5 | 50.9 | 51.0 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 | 9.00 | 9.00 | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 5.28 | 5.28 | 28.6 | 28.6 | 5.95 | 5.97 |
| N=20 headless idle | rss_mib | 8.15 | 8.27 | 28.6 | 28.6 | 13.7 | 13.8 |
| N=20 headless output | cpu_pct | 3.80 | 4.00 | 21.4 | 25.8 | 4.70 | 5.20 |
| N=20 headless output | pss_mib | 5.29 | 5.31 | 28.9 | 34.3 | 5.93 | 6.00 |
| N=20 headless output | rss_mib | 8.17 | 8.29 | 28.9 | 34.3 | 13.6 | 13.8 |
| N=50 attached idle | cpu_pct | 0.00 | 0.00 | 70.1 | 73.2 | 11.5 | 11.8 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| N=50 attached idle | pss_mib | 16.5 | 16.5 | 72.6 | 86.7 | 48.3 | 87.4 |
| N=50 attached idle | rss_mib | 24.0 | 24.1 | 82.6 | 96.7 | 62.9 | 150 |
| N=50 attached output | cpu_pct | 9.36 | 9.97 | 85.2 | 102 | 38.7 | 39.7 |
| N=50 attached output | pss_mib | 16.6 | 17.5 | 80.5 | 95.6 | 46.9 | 49.0 |
| N=50 attached output | rss_mib | 24.1 | 24.3 | 90.5 | 106 | 61.4 | 63.2 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 | 22.4 | 22.6 | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 5.26 | 5.30 | 45.9 | 49.5 | 5.87 | 5.87 |
| N=50 headless idle | rss_mib | 8.23 | 8.30 | 45.9 | 49.5 | 13.7 | 13.7 |
| N=50 headless output | cpu_pct | 10.4 | 11.5 | 55.7 | 68.8 | 11.9 | 12.1 |
| N=50 headless output | pss_mib | 5.29 | 5.31 | 50.4 | 59.1 | 5.91 | 5.93 |
| N=50 headless output | rss_mib | 8.25 | 8.31 | 50.4 | 59.1 | 13.7 | 13.8 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 0.98 | 1.07 | 2.16 | 2.31 | 24.8 | 48.2 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.82 | 1.08 | 0.48 | 0.69 | 42.1 | 43.0 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
