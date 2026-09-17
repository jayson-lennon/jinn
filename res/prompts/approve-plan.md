+++
name = "approve-plan"
description = "Generate a context-rich implementation specification from an approved plan."
+++

<instructions>

The user has approved the plan discussed previously. Your task is to create the **Context-Rich Specification**, review it, and then create the implementation task list.

## Instructions

1.  **Synthesize History:** Review the entire Socratic dialogue and the approved plan. Extract all technical decisions, code references, and algorithms discussed.
2.  **Generate Specification:** Create a comprehensive, standalone technical specification and save using the `save_plan` tool to `.plans/<task>/plan.md` (where `<task>` is a slugified version of the task name). The specification is the "spec" — the authoritative reference for what to implement.
3.  **Review Specification:** Re-read the generated spec end-to-end. Verify that every phase from the approved plan is covered, that all mandatory sections are present, and that a fresh agent could implement the feature using only this document. Fix any gaps, inconsistencies, or ambiguities you find.
4.  **Create Todo List:** Use the todo list tools to populate the todo list with the phases and tasks from the approved plan. Each phase should have a short description (e.g., "Research", "Build", "Test") and an ordered list of task descriptions. Use the same phases that appear in the spec. **Append a final phase called "Verification"** with each acceptance criterion from the approved plan as a separate task in that phase. This ensures the agent verifies all acceptance criteria before declaring the task complete.
    - The todo list entries should _not_ encompass large amounts of work. Create fine-grained todo entries as-needed to properly track the overall task.
    - The "Verification" phase must include "Record Updates" at the end iff the approved plan contains a "Record Updates" section. The task description must encode this two-part logic: (1) review the complete implementation; (2a) if the implementation matches the planned Record Updates, write those exact entries into `.agents/RECORD.md`; (2b) if the implementation **diverged** from the planned entries, **do not write a wrong entry** — instead surface the divergence in your final implementation summary so the user can resolve it.
5.  **Report:** Reply to the user with "Plan created at .plans/<task>/plan.md and task list initialized."

## Mandatory Elements

The specification must be a **standalone technical document**. A fresh agent with no prior memory must be able to implement the feature using only this document. It MUST include the following sections explicitly:

1.  **Dialectical Outcomes (Why):** Reasoning for key decisions based on the Socratic dialogue. Document trade-offs and alternatives rejected.
2.  **Relevant Files (Where):** A list of specific files to be created or modified, with full paths.
3.  **Key Code Context (What):** Snippets of existing code that the implementation depends on or must modify (e.g., struct definitions, function signatures). Do not just reference them; include the code blocks.
4.  **Implementation Algorithm (How):** The explicit logic for implementation. Detail state machines, logic flows, or data transformations.
5.  **Anti-Goals (Out of Scope):** Explicitly list what is _not_ being implemented to prevent scope creep.
6.  **Edge Cases & Gotchas:** Highlight technical pitfalls or tricky logic discovered during the dialogue.
7.  **Navigation Anchors:** Identify specific functions or modules that serve as primary entry points for the changes.
8.  **Dependency Mappings:** List new external libraries or internal module dependencies required.
9.  **Test Strategies:** Specific guidance on _how_ to verify each phase (e.g., "Update unit test X", "Add test for edge case Y").

## Structure

- The document **must** begin with the "Problem" and "Solution" from the approved plan.
- The document **must** include the "Acceptance Criteria" from the approved plan.
- The document **must** include the "Phases" from the approved plan, expanded with implementation specifics describing what each phase covers. Do not use checkboxes for status tracking — the task list handles that.
- The document **must** have dedicated headers for all Mandatory Elements listed above.

</instructions>
