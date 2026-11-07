# Multiplexer benchmark — summary

Run 20260925T104415Z · reps 5 (+1 warm-up discarded)

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

## throughput

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| attached | host_cpu_s | 0.23 | 0.24 | 0.19 | 0.20 | 0.38 | 0.39 |
| attached | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| attached | producer_ms | 233 | 233 | 145 | 145 | 246 | 248 |
| attached | pss_after_mib | 7.73 | 7.74 | 23.2 | 25.0 | 37.1 | 37.1 |
| attached | settle_ms | 445 | 465 | 651 | 1143 | 377 | 481 |
| attached | visible_ms | 334 | 337 | 158 | 162 | 269 | 293 |
| headless | host_cpu_s | 0.21 | 0.21 | 0.13 | 0.13 | 0.21 | 0.22 |
| headless | intact | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| headless | producer_ms | 210 | 213 | 128 | 129 | 213 | 214 |
| headless | pss_after_mib | 5.21 | 5.21 | 25.3 | 26.1 | 9.95 | 9.95 |
| headless | settle_ms | 338 | 453 | 255 | 260 | 343 | 352 |

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 1.04 | 1.10 | 2.23 | 2.34 | 4.13 | 5.56 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.76 | 0.95 | 0.45 | 0.55 | 1.90 | 2.90 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
