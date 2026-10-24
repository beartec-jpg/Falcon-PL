# Packer vs UNL isolation dig

**Verdict:** PRIMARY: multipack still helps at N=6. UNL-only: larger UNL under single-packer still loses TPS (cert/quorum tax).

| N | multipack best min | multipack tight | single-packer best min | single tight |
|---|--------------------|-----------------|------------------------|--------------|
| 3 | 46.18 | 46.18 | 22.69 | 22.69 |
| 6 | 14.49 | 8.37 | 5.71 | 5.71 |

## Phases

### N=3
- multipack: `{'warm': 8.79, 'light': 22.7, 'med': 46.18}`
- single: `{'warm': 8.81, 'light': 22.69, 'med': 12.95}`

### N=6
- multipack: `{'warm': 8.37, 'light': 8.89, 'med': 14.49}`
- single: `{'warm': 5.71, 'light': 3.77, 'med': 2.39}`

