
## Query latency by corpus size (p50 / p99, µs; recall@10 where applicable)

### keyword

| docs | bog p50 | bog p99 | sqlite p50 | sqlite p99 | p50 speedup | bog recall | sqlite recall |
|---|---|---|---|---|---|---|---|
| 1000 | 22.7 | 29.4 | 101.1 | 127.0 | 4.5× | — | — |
| 10000 | 280.3 | 349.0 | 363.7 | 452.4 | 1.3× | — | — |
| 100000 | 7878.8 | 10615.1 | 3440.5 | 4642.2 | 0.4× | — | — |

### semantic

| docs | bog p50 | bog p99 | sqlite p50 | sqlite p99 | p50 speedup | bog recall | sqlite recall |
|---|---|---|---|---|---|---|---|
| 1000 | 116.2 | 160.8 | 659.8 | 924.3 | 5.7× | 1.000 | 1.000 |
| 10000 | 135.5 | 228.8 | 8757.2 | 9414.0 | 64.6× | 0.993 | 1.000 |
| 100000 | 401.3 | 527.5 | 91558.8 | 133589.0 | 228.1× | 0.967 | 1.000 |

### hybrid

| docs | bog p50 | bog p99 | sqlite p50 | sqlite p99 | p50 speedup | bog recall | sqlite recall |
|---|---|---|---|---|---|---|---|
| 1000 | 132.5 | 163.3 | 762.7 | 998.3 | 5.8× | — | — |
| 10000 | 438.6 | 605.1 | 8997.2 | 10095.7 | 20.5× | — | — |
| 100000 | 7835.7 | 10016.1 | 90274.0 | 100371.5 | 11.5× | — | — |

### stats

| docs | bog p50 | bog p99 | sqlite p50 | sqlite p99 | p50 speedup | bog recall | sqlite recall |
|---|---|---|---|---|---|---|---|
| 1000 | 0.2 | 0.3 | 36.5 | 60.2 | 175.5× | — | — |
| 10000 | 0.2 | 0.3 | 370.8 | 797.3 | 1483.2× | — | — |
| 100000 | 0.3 | 0.3 | 12968.8 | 37513.8 | 44413.7× | — | — |

## Churn: query latency mid-write-storm (p50 / p99, µs)

| ops applied | system | op | p50 | p99 | recall@10 |
|---|---|---|---|---|---|
| 1000 | bog | keyword | 7526.2 | 9614.0 | — |
| 1000 | bog | semantic | 374.0 | 522.5 | 0.945 |
| 1000 | bog | hybrid | 7607.4 | 9130.9 | — |
| 1000 | bog | stats | 0.3 | 0.3 | — |
| 1000 | sqlite | keyword | 3517.8 | 4067.6 | — |
| 1000 | sqlite | semantic | 88835.7 | 127608.3 | 1.000 |
| 1000 | sqlite | hybrid | 95391.6 | 545548.5 | — |
| 1000 | sqlite | stats | 12954.5 | 15966.8 | — |
| 2000 | bog | keyword | 7675.2 | 8908.4 | — |
| 2000 | bog | semantic | 397.9 | 532.9 | 0.932 |
| 2000 | bog | hybrid | 7826.1 | 9080.2 | — |
| 2000 | bog | stats | 0.3 | 0.4 | — |
| 2000 | sqlite | keyword | 3183.7 | 3974.5 | — |
| 2000 | sqlite | semantic | 86388.2 | 129000.2 | 1.000 |
| 2000 | sqlite | hybrid | 89712.8 | 212360.6 | — |
| 2000 | sqlite | stats | 12121.4 | 15462.2 | — |
| 3000 | bog | keyword | 8569.2 | 61185.9 | — |
| 3000 | bog | semantic | 538.3 | 1670.3 | 0.940 |
| 3000 | bog | hybrid | 7699.6 | 9964.2 | — |
| 3000 | bog | stats | 0.3 | 0.4 | — |
| 3000 | sqlite | keyword | 3514.2 | 4108.1 | — |
| 3000 | sqlite | semantic | 88421.8 | 256601.6 | 1.000 |
| 3000 | sqlite | hybrid | 89980.0 | 95073.9 | — |
| 3000 | sqlite | stats | 12873.7 | 70023.5 | — |
| 4000 | bog | keyword | 6918.7 | 9988.0 | — |
| 4000 | bog | semantic | 349.0 | 498.0 | 0.932 |
| 4000 | bog | hybrid | 7101.4 | 8250.3 | — |
| 4000 | bog | stats | 0.3 | 0.4 | — |
| 4000 | sqlite | keyword | 3754.1 | 4929.5 | — |
| 4000 | sqlite | semantic | 88295.9 | 186781.6 | 1.000 |
| 4000 | sqlite | hybrid | 90693.4 | 195449.5 | — |
| 4000 | sqlite | stats | 9355.0 | 14173.4 | — |

## Ingest (SQLite's row to win)

| docs reached | system | docs/sec | batch p50 ms | batch p99 ms |
|---|---|---|---|---|
| 1000 | bog | 1319 | 758.06 | 758.06 |
| 1000 | sqlite | 15664 | 63.84 | 63.84 |
| 10000 | bog | 934 | 1101.46 | 1255.34 |
| 10000 | sqlite | 13705 | 74.12 | 85.43 |
| 100000 | bog | 544 | 1738.80 | 2550.52 |
| 100000 | sqlite | 12358 | 77.86 | 116.98 |
| 4000 | bog-churn | 635 | 11.66 | 23.99 |
| 4000 | sqlite-churn | 1835 | 2.61 | 49.76 |

## Disk

| stage | system | MB |
|---|---|---|
| after ingest | bog | 938.4 |
| after ingest | sqlite | 349.8 |
| after churn | bog | 1085.5 |
| after churn | sqlite | 359.6 |
