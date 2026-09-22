---
name: PARSER_VERSION bump rules
description: When parse.rs changes require a PARSER_VERSION bump vs. when a migration backfill is sufficient
type: project
---

The parse stage re-parses any `statsbooks` row with `parser_version < PARSER_VERSION` (`parse_pending` in `src/ingest/statsbook_parse.rs`). This only matters for changes that affect the parsed `statsbook_games` content.

**Why:** Re-parsing re-downloads the bytes from Google Drive and re-parses the workbook, so it is the right path for parsing-logic changes but pointless overhead for anything else.

**How to apply:**
- Bump PARSER_VERSION when: parsing logic that produces `GameData` changes (e.g., SP-row handling, score calculation, new fields extracted from xlsx). Existing statsbooks must be re-parsed to gain the fix/new data.
- Do NOT bump for: changes outside `parse.rs` — schema/search columns, handlers, templates, or the matcher. Those are not parser output, so a re-parse would change nothing.
- A diff to `parse.rs` that only adds tracing/logging or refactors without behavior change does NOT need a bump.
