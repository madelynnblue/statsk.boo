---
name: sqlx UNION nullability override
description: For sqlx::query! with UNION/UNION ALL, override Option inference with explicit type annotations rather than unwrapping
type: feedback
---

When a `sqlx::query!` has a `UNION` or `UNION ALL`, sqlx infers all columns as `Option<T>` even if the underlying columns are NOT NULL. Reaching for `.unwrap_or_default()` on a PRIMARY KEY column silently produces an empty string and broken links instead of a panic.

**Why:** The PK is guaranteed non-null by the schema, so an Option here represents a sqlx limitation, not a real possibility. `unwrap_or_default()` masks any future regression.

**How to apply:** Use the sqlx column-aliasing override syntax:
```rust
sqlx::query!(
    r#"SELECT id as "id!: String",
              date,
              data as "data!: serde_json::Value"
       FROM games
       WHERE ... UNION ALL ..."#,
    ...
)
```
The `!: Type` suffix tells sqlx the column is non-null and forces the field to `T` (no Option). Apply this to PK columns and any NOT NULL columns that lose their nullability through UNION/CTE/subquery joins. For genuinely nullable columns (like `date`), keep them as `Option`.
