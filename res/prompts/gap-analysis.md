+++
name = "gap-analysis"
description = "Check for gaps in the implementation versus the acceptance criteria"
+++

<instructions>
Verify the implementation against the approved spec/plan and discussion, then report in the compact contract below. Every verification duty still applies: check **every** acceptance criterion, check the plan's expected features (a missed feature is a gap), and reconcile the Record.

## Report contract

The report has exactly two shapes. Never emit tables. Never emit a Recommendations section — the `resolution:` line in each gap is the recommendation. Order gaps most-important-first. Keep each `expected:`, `gap:`, and `resolution:` line to a single sentence.

### 1. Per-AC verification line

One line per acceptance criterion, then nothing else about it:

```
AC1: met — <one-sentence evidence>
AC2: unmet — <one-sentence evidence>
```

A missed expected feature never gets its own line — it becomes a gap block.

### 2. Gap block

```
**G1 — <title>**
expected: <what the spec required>
gap: <what is missing or wrong>
resolution: <specific action: the file, test, or record entry to change>
```

Number gaps G1, G2, … in priority order. If there are no gaps, the report is the single line `No gaps.`

## Record reconciliation

If the spec/plan included "Record Updates", also **confirm accuracy against the actual implementation**. Read `.agents/RECORD.md` if it exists. The implementer writes Record updates at the end of implementation (via the "Update the Record" task), so by the time you check, the Record should already reflect this work.

- For each "Record Updates" entry the approved plan promised, confirm it was written into `.agents/RECORD.md` and that it **matches what was actually implemented**. If it was not written, or if what was written does not match the implementation, flag the omission/mismatch as a gap block.
- If the implementer surfaced a **divergence** (implementation did not match the planned entries, so it wrote nothing), verify that divergence is genuine, then propose a correct verbatim entry as the gap's `resolution:` line for the user to approve.
- For any recorded entry the implementation changed, broke, or made stale that was **not** covered by the planned Record Updates, flag it as a gap block and propose the exact amended (or removed) entry verbatim as its `resolution:` line.
- If the work established a new high-level fact that has no entry yet, propose a verbatim entry as a gap block's `resolution:` line, following the record's format rules.
- Do not flag cosmetic or unrelated edits; only surface entries whose stated behavior diverged or was newly established.
</instructions>
