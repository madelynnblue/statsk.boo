---
name: PARSER_VERSION bump rules
description: When parse.rs changes require a PARSER_VERSION bump vs. when a migration backfill is sufficient
type: project
---

The ingester re-parses any row with `parser_version < PARSER_VERSION` on startup (`reingest_stale`). This only matters for changes that affect the JSONB `data` column.

**Why:** Re-ingest re-downloads from Google Drive and re-parses, so it's the right path for parsing-logic changes but pointless overhead for denormalization-column changes.

**How to apply:**
- Bump PARSER_VERSION when: parsing logic that produces `GameData` changes (e.g., SP-row handling, score calculation, new fields extracted from xlsx). Existing rows must be re-parsed to gain the fix/new data.
- Do NOT bump for: new denormalized search columns (e.g., `league_search`) backfilled by migration SQL, since the JSONB content is unchanged and a UPDATE statement is cheaper than re-downloading every file.
- A diff to `parse.rs` that only adds tracing/logging or refactors without behavior change does NOT need a bump.
