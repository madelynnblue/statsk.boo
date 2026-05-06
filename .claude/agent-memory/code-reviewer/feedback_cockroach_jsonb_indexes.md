---
name: CockroachDB JSONB indexing rules
description: CockroachDB silently accepts B-tree on JSONB but does NOT use it for equality lookups (full scan); use inverted (GIN) index + `=` or `@>` instead
type: feedback
---

CockroachDB **silently accepts** `CREATE INDEX foo ON t (jsonb_col)` (plain B-tree on JSONB) without error, but the planner **does not use it** for `WHERE jsonb_col = X` lookups — those queries fall back to a full table scan. CockroachDB even emits an `index recommendations: ... CREATE INVERTED INDEX` hint in `EXPLAIN`.

Verified empirically: with 1000 rows and a B-tree index on the JSONB column, `EXPLAIN SELECT ... WHERE data = '...'::JSONB` shows `spans: FULL SCAN` on the primary key. With an inverted index instead, the same equality query uses the inverted index as a pre-filter then index-joins to the PK (~600µs vs full scan).

**Why:** CockroachDB's planner does not have a B-tree-on-JSONB equality access path implemented. Inverted indexes are the only indexed access path for JSONB. Equality comparisons over inverted indexes work (the planner emits a span scan over the inverted entries that match), as does `@>` containment.

**How to apply:** When reviewing a migration in this repo (CockroachDB target), flag any `CREATE INDEX ... ON tbl (jsonb_col)` without `USING GIN` (= inverted). The indexed query can use either `=` or `@>` — both will use the inverted index. Run `EXPLAIN` (against a populated table — small tables prefer scan) to confirm the index is actually used.
