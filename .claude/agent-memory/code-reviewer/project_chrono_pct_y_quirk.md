---
name: chrono-pct-y-quirk
description: chrono %Y accepts 1-4 digit years on parse — "08/06/24" silently becomes year 0024 (AD), not 2024
metadata:
  type: project
---

chrono's `%Y` (and `%m/%d/%Y` / `%d/%m/%Y`) parse 1-4 digits for the year, so a 2-digit-year text date like "08/06/24" or "Sept 7 24" silently parses as year 24 AD (0024), not 2024. Verified against chrono 0.4.44 source (`scan::number(s, 1, width)` with width=4 for `Year`) and empirically. `parse_from_str` otherwise requires the full string to match (trailing junk is rejected).

**Why:** This bites the text-date recovery in `src/ingest/parse.rs` (`parse_text_date` / `parse_month_name_date`): a wrong year yields a wrong `canonical_id` fingerprint (date is part of the hash) and a wrong date on the site, and the ingest future-date check doesn't catch it (AD 24 is in the past). Any year-sanity check should reject years outside a plausible range (e.g. < 1990 or > now+1) so the file-name date fallback (which is correct) gets used instead.

**How to apply:** When reviewing or extending date parsing in parse.rs, treat a 2-digit-year acceptance as a defect, and prefer `from_ymd_opt` range checks over format-string strictness. Related: [[project_parser_version_bumps]] — date parsing changes count as parsing logic changes.
