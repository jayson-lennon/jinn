+++
name = "approve-plan"
description = "Generate a context-rich implementation specification from an approved plan."
+++

<instructions>

The user has approved the plan discussed previously. Your task is to create the **Context-Rich Specification**, review it, and then create the implementation task list.

## Instructions

1.  **Synthesize History:** Review the entire Socratic dialogue and the approved plan. Extract all technical decisions, code references, and algorithms discussed.
2.  **Carry the Decision Ledger verbatim.** If the approved plan has a Decision Ledger table, copy it into the spec unchanged — the same rows, the same cells, the same `Chose` values. Do not improve the wording of a Chosen cell, do not fill in a row the dialogue did not settle, and do not resolve an `agent` row into a `user` one because the decision looks settled now. Where the dialogue closed a question in the user's own words, those words are the Chosen cell.
3.  **Generate Specification:** Create a comprehensive, standalone technical specification and save using the `save_plan` tool to `.plans/<task>/plan.md` (where `<task>` is a slugified version of the task name). The specification is the "spec" — the authoritative reference for what to implement.
4.  **Review Specification:** Re-read the generated spec end-to-end. Verify that every phase from the approved plan is covered, that all mandatory sections are present, and that a fresh agent could implement the feature using only this document. Fix any gaps, inconsistencies, or ambiguities you find.

    **Then audit every decision in the spec against the ledger.** For each row, find the place in the spec that implements it and confirm the spec says what was chosen. A decision that appears in the spec but not the ledger is one you invented during synthesis — mark it `agent` in the ledger or cut it. A ledger row whose `Chose` is `user` but whose implementation reads differently is a substitution: fix the spec to match the decision, never the reverse.
5.  **Create Todo List:** Use the todo list tools to populate the todo list with the phases and tasks from the approved plan. Each phase should have a short description (e.g., "Research", "Build", "Test") and an ordered list of task descriptions. Use the same phases that appear in the spec. **Append a final phase called "Verification"** with each acceptance criterion from the approved plan as a separate task in that phase. This ensures the agent verifies all acceptance criteria before declaring the task complete.
    - The todo list entries should _not_ encompass large amounts of work. Create fine-grained todo entries as-needed to properly track the overall task. Prefer numerous small entries instead of 1 large entry.
    - Every task that implements a `user`-chosen ledger row names that choice in the task description, so the task list carries the decision alongside the work.
    - The "Verification" phase must include "Record Updates" at the end iff the approved plan contains a "Record Updates" section. The task description must encode this two-part logic: (1) review the complete implementation; (2a) if the implementation matches the planned Record Updates, write those exact entries into `.agents/RECORD.md`; (2b) if the implementation **diverged** from the planned entries, **do not write a wrong entry** — instead surface the divergence in your final implementation summary so the user can resolve it.
6.  **Report and confirm.** State where the plan was written and that the task list is initialized, then reproduce the Decision Ledger and ask the user to confirm each row names the option _they_ chose. This is the last point at which a substitution is cheap to catch — the conversation is about to be discarded and the implementer will see only the spec. Wait for the confirmation before declaring the work ready.

## Mandatory Elements

The specification must be a **standalone technical document**. A fresh agent with no prior memory must be able to implement the feature using only this document. It MUST include the following sections explicitly:

1.  **Dialectical Outcomes (Why):** Reasoning for key decisions based on the Socratic dialogue. Document trade-offs and alternatives rejected.
2.  **Decision Ledger:** The table carried verbatim from step 2, unedited. Where the spec implements a `user`-chosen row, name the section that carries it.
3.  **Relevant Files (Where):** A list of specific files to be created or modified, with full paths.
4.  **Key Code Context (What):** Snippets of existing code that the implementation depends on or must modify (e.g., struct definitions, function signatures). Do not just reference them; include the code blocks.
5.  **Implementation Algorithm (How):** The explicit logic for implementation. Detail state machines, logic flows, or data transformations.
6.  **Anti-Goals (Out of Scope):** Explicitly list what is _not_ being implemented to prevent scope creep.
7.  **Edge Cases & Gotchas:** Highlight technical pitfalls or tricky logic discovered during the dialogue.
8.  **Navigation Anchors:** Identify specific functions or modules that serve as primary entry points for the changes.
9.  **Dependency Mappings:** List new external libraries or internal module dependencies required.
10.  **Test Strategies:** Specific guidance on _how_ to verify each phase (e.g., "Update unit test X", "Add test for edge case Y").

## Structure

- The document **must** begin with the "Problem" and "Solution" from the approved plan.
- The document **must** include the "Acceptance Criteria" from the approved plan.
- The document **must** include the "Phases" from the approved plan, expanded with implementation specifics describing what each phase covers. Do not use checkboxes for status tracking — the task list handles that.
- The document **must** have dedicated headers for all Mandatory Elements listed above.

</instructions>
