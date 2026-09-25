# Multiplexer benchmark — summary

Run 20260925T083849Z · reps 5 (+1 warm-up discarded)

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
    "herdr": "herdr 0.9.1",
    "thurbox": "0.0.0-dev (schema v47)",
    "python": "3.13.15"
  }
}
```

## latency

| variant | metric | herdr median | herdr p95 | thurbox median | thurbox p95 |
|---|---|---|---|---|---|
| idle | echo_ms | 2.18 | 2.34 | 3.85 | 5.55 |
| idle | timeouts | 0.00 | 0.00 | 0.00 | 0.00 |
| other-busy | echo_ms | 0.45 | 0.54 | 1.84 | 2.96 |
| other-busy | timeouts | 0.00 | 0.00 | 0.00 | 0.00 |
