# Golden ledger

`goldens/` preserves test evidence from the first engine and keeps an inventory
of test-bearing source files. It remains useful when a current behavior is
compared with its predecessor, without making the deleted implementation part of
the active product documentation.

```bash
cargo xtask goldens
cargo xtask goldens --check
```

The first command refreshes mined metadata while preserving manual
classifications. The second is the CI drift check.

## Source inventory

`goldens/sources.json` lists legacy sources that were mined and current test
paths that are deliberately excluded. Each exclusion carries a reason. The check
fails when a test-bearing file is absent from both lists, which keeps new test
locations visible during review.

A mined scenario records the operations, request literals, fixtures, file-level
data, and category found in its original test. The miner reads source syntax; it
does not execute the deleted test suite.

## Classifications

Each scenario has one status:

- `pending`: historical evidence that has not been mapped to a current test;
- `covered`: current coverage exists and `covered_by` names it;
- `dropped`: the behavior was intentionally removed and `reason` explains why.

`pending` entries are retained as research material. They do not define beta
scope or release readiness; the accepted ADRs, spec coverage ledger,
conformance suite, and live round-trip gates do that.

## What CI checks

1. Mined entries still match their recorded source metadata.
2. Every test-bearing source path is listed or excluded.
3. Status fields are valid, `covered` has `covered_by`, and `dropped` has a
   reason.

A deleted legacy test keeps its scenario. Remove a source entry only when the
corresponding current path no longer needs inventory coverage.
