# व्यापार・账簿 tantra-ledger

> *Domain Heart (Zero-I/O): Order Management System (OMS), FIFO tax-lot portfolio tracking, and double-entry general ledger.*

Zero-I/O pure state machine domain engine enforcing double-entry bookkeeping, multi-currency position tracking, and atomic OMS state transitions.

## Subcrates

- `tantra-ledger` — Facade and aggregate coordinator
- `tantra-ledger-oms` — Order Management System state machine & transitions
- `tantra-ledger-portfolio` — FIFO tax-lot accounting and position tracking
- `tantra-ledger-journal` — Double-entry general ledger and balance validation

## Build & Test

```bash
make prod
make check
make test
```

License: AGPL-3.0.
