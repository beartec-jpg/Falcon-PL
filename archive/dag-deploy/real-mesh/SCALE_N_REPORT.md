# Phase A — Throughput vs N (horizontal scale)

OUT: `/tmp/packer-vs-unl-20260810-091449/single`
Transport: LAN `192.0.2.1` / `192.0.2.2` · crypto=falcon-512 · knobs v1-best

**Verdict:** FAIL: tight multi-host throughput does NOT grow with N (or insufficient tight passes)

## Curve (best exec_tps_min per N)

| N | placement f1/f2 | best exec_tps_min | best phase | tight exec_tps_min | tight phase | final spread |
|---|-----------------|-------------------|------------|--------------------|-------------|--------------|
| 3 | [1, 2]/[3] | **22.69** | light | 22.69 | light | 84 |
| 6 | [1, 2, 3]/[4, 5, 6] | **5.71** | warm | 5.71 | warm | 0 |

## Per-N ladder (exec_tps_min)

| N | warm(10) | light(25) | med(50) | mid(80) |
|---|----------|-----------|---------|---------|
| 3 | 8.81 | 22.69 | 12.95 | — |
| 6 | 5.71 | 3.77 | 2.39 | — |

## Scale-out analysis
```json
{
  "series_best_exec_tps_min": [
    [
      3,
      22.69
    ],
    [
      6,
      5.71
    ]
  ],
  "series_tight_exec_tps_min": [
    [
      3,
      22.69
    ],
    [
      6,
      5.71
    ]
  ],
  "scale_out_pass_tight": false,
  "scale_out_detail_tight": {
    "overall_up": false,
    "non_decreasing_steps": 0,
    "steps": 1,
    "series": [
      [
        3,
        22.69
      ],
      [
        6,
        5.71
      ]
    ]
  },
  "scale_out_pass_best_effort": false,
  "scale_out_detail_best": {
    "overall_up": false,
    "non_decreasing_steps": 0,
    "steps": 1,
    "series": [
      [
        3,
        22.69
      ],
      [
        6,
        5.71
      ]
    ]
  },
  "verdict": "FAIL: tight multi-host throughput does NOT grow with N (or insufficient tight passes)"
}
```

## Anti-fudge notes
- Same binary/knobs for all N
- `exec_tps_min` = slowest UNL validator delta (not fastest)
- Owner-route by FNV lane (real pack ownership model)
- 2 archivals always present (order-exec, no pack)
- LAN multi-host (not localhost-only)

