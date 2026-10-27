# ledger

A small double-entry bookkeeping toolkit.

* `ledger-core`: amounts (exact, in cents), accounts and balanced transactions.
* `ledger-import`: reads bank statements exported as CSV into transactions.
* `ledger-report`: balances, category roll-ups and a plain-text statement.

Amounts are parsed from decimal text (`12`, `12.5`, `1,234.56`, `-3.10`), and accounting-style
negatives in parentheses (`(4.00)`) are accepted by the importer. Every transaction must balance:
the postings of one transaction sum to zero.

```sh
cargo test --workspace
```
