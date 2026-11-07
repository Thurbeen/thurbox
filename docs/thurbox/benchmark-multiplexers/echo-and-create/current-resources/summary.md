# Multiplexer benchmark — summary

Run 20260925T102629Z · reps 5 (+1 warm-up discarded)

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
    "thurbox": "0.0.0-dev (schema v47)",
    "python": "3.13.15"
  }
}
```

## resources

| variant | metric | thurbox median | thurbox p95 |
|---|---|---|---|
| N=1 attached idle | cpu_pct | 2.90 | 3.10 |
| N=1 attached idle | first_view_stale | 0.00 | 0.00 |
| N=1 attached idle | pss_mib | 28.5 | 28.6 |
| N=1 attached idle | rss_mib | 42.6 | 42.6 |
| N=1 attached output | cpu_pct | 8.00 | 8.10 |
| N=1 attached output | pss_mib | 28.8 | 28.8 |
| N=1 attached output | rss_mib | 42.8 | 42.9 |
| N=1 headless idle | cpu_pct | 0.00 | 0.00 |
| N=1 headless idle | pss_mib | 8.24 | 8.24 |
| N=1 headless idle | rss_mib | 17.1 | 17.1 |
| N=1 headless output | cpu_pct | 0.20 | 0.20 |
| N=1 headless output | pss_mib | 8.24 | 8.24 |
| N=1 headless output | rss_mib | 17.1 | 17.1 |
| N=20 attached idle | cpu_pct | 5.39 | 5.49 |
| N=20 attached idle | first_view_stale | 0.00 | 0.00 |
| N=20 attached idle | pss_mib | 36.2 | 36.3 |
| N=20 attached idle | rss_mib | 50.2 | 50.4 |
| N=20 attached output | cpu_pct | 19.0 | 19.3 |
| N=20 attached output | pss_mib | 36.4 | 36.7 |
| N=20 attached output | rss_mib | 50.5 | 50.8 |
| N=20 headless idle | cpu_pct | 0.00 | 0.00 |
| N=20 headless idle | pss_mib | 8.58 | 8.58 |
| N=20 headless idle | rss_mib | 17.4 | 17.4 |
| N=20 headless output | cpu_pct | 4.20 | 4.70 |
| N=20 headless output | pss_mib | 8.66 | 8.66 |
| N=20 headless output | rss_mib | 17.5 | 17.5 |
| N=50 attached idle | cpu_pct | 9.45 | 9.48 |
| N=50 attached idle | first_view_stale | 0.00 | 0.00 |
| N=50 attached idle | pss_mib | 46.9 | 56.7 |
| N=50 attached idle | rss_mib | 61.0 | 92.3 |
| N=50 attached output | cpu_pct | 35.3 | 38.1 |
| N=50 attached output | pss_mib | 47.2 | 47.2 |
| N=50 attached output | rss_mib | 61.3 | 61.3 |
| N=50 headless idle | cpu_pct | 0.00 | 0.00 |
| N=50 headless idle | pss_mib | 9.06 | 9.06 |
| N=50 headless idle | rss_mib | 17.9 | 17.9 |
| N=50 headless output | cpu_pct | 8.90 | 9.40 |
| N=50 headless output | pss_mib | 9.33 | 9.33 |
| N=50 headless output | rss_mib | 18.2 | 18.2 |
