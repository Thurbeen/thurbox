# Multiplexer benchmark — summary

Run 20260924T012237Z · reps 5 (+1 warm-up discarded)

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

## latency

| variant | metric | tmux median | tmux p95 | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|---|---|
| idle | echo_ms | 1.02 | 1.08 | 2.16 | 2.33 | 3.47 | 4.86 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.74 | 0.93 | 0.45 | 0.52 | 1.73 | 2.37 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
