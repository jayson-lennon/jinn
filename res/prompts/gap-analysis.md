+++
name = "gap-analysis"
description = "Check for gaps in the implementation versus the acceptance criteria"
+++

<instructions>

Verify the implementation against the approved spec/plan and discussion.

## Read the spec

Read the plan/spec from the `.plans/` directory. There may be multiple from other sessions; only read those that were just implemented.

After reading the plan/spec, browse the codebase and use your context history to ensure that the Acceptance Criteria section was actually met. Evaluate each entry in the Acceptance Criteria 1 by 1 to ensure compliance.

## Record reconciliation

If the spec/plan included "Record Updates", also **confirm accuracy against the actual implementation**. Read `.agents/RECORD.md` if it exists. The implementer writes Record updates at the end of implementation (via the "Update the Record" task), so by the time you check, the Record should already reflect this work.

- For each "Record Updates" entry the approved plan promised, confirm it was written into `.agents/RECORD.md` and that it **matches what was actually implemented**. If it was not written, or if what was written does not match the implementation, include this information in the "Record reconciliation" section.
- For any recorded entry the implementation changed, broke, or made stale that was **not** covered by the planned Record Updates, include this information in the "Record reconciliation" section.
- If the work established a new high-level fact that has no entry yet, propose a verbatim entry in the "Record reconciliation" section.
- Do not flag cosmetic or unrelated edits; only surface entries whose stated behavior diverged or was newly established.

<example>
# Gap analysis report

## AC1: Foo the bar

- **Status:** ✅ met
- **Evidence:** The `foo` method takes a `bar` parameter.

## AC2: Add component adds both negative and positive numbers

- **Status:** ❌ unmet
- **Evidence:** The `add` component accepts a `u32` datatype which cannot represent negative numbers.
- **Recommendation:** Change the datatype to `i32` to add negative number support.

## AC3: Docs updated with an easier usage example

- **Status:** ⚠️ partial
- **Evidence:** The docs were updated, but the usage example still has high cyclomatic complexity.
- **Recommendation:** Split the usage example into two smaller examples.

## Record reconciliation recommendations

- The implementation changed the display for the calculator.
  - Add: `(calculator) Display now uses a high-contrast font`
- The button colors were changed to black and white.
  - Remove: `(calculator) Number buttons use rainbow colors`

</example>
</instructions>
