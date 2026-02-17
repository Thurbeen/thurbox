# Refactor

Perform a multi-pass refactoring of the newly implemented
code in this project. You MUST execute 3 independent review
passes, re-reading the code from scratch each time to catch
incoherences that a single pass would miss.

---

## Pre-work

**Identify recent changes**: Look at recently modified files
(use git diff or git log) to find newly implemented code.
Establish the list of files to review.

---

## Pass 1 — Structure & Clean Code

Re-read all identified files from disk, then:

1. **Apply Clean Code principles**:
   - Use clear, intention-revealing names for variables,
     functions, and classes.
   - Keep functions small — each should do one thing well
     (Single Responsibility).
   - Eliminate duplication (DRY).
   - Remove dead code, unused imports, and unnecessary
     comments.
   - Replace magic numbers/strings with named constants.

2. **Keep It Simple (KISS)**:
   - Prefer straightforward logic over clever tricks.
   - Reduce nesting — use early returns and guard clauses.
   - Avoid premature abstractions — only extract when there
     is real duplication.
   - Minimize function arguments; group related parameters
     into objects if needed.

3. **Improve readability**:
   - Ensure consistent formatting and code style with the
     rest of the project.
   - Structure code so it reads top-down.
   - Let the code be self-documenting; only add comments for
     non-obvious "why", never for "what".

Apply fixes, then summarize what was changed in Pass 1.

---

## Pass 2 — Coherence & Consistency

Re-read ALL the same files again from disk (fresh read, do
not rely on memory from Pass 1), then:

1. **Cross-file coherence**: Check that naming conventions,
   patterns, and abstractions are consistent across all
   changed files and with the rest of the project.
2. **API & contract consistency**: Verify that function
   signatures, return types, error handling, and data shapes
   are coherent between callers and callees.
3. **Logic review**: Look for contradictory logic, redundant
   conditions, unreachable branches, or mismatched
   assumptions between modules.
4. **Import & dependency hygiene**: Ensure no circular
   dependencies, unused imports, or misplaced
   responsibilities were introduced.

Apply fixes, then summarize what was changed in Pass 2.

---

## Pass 3 — Tests & Final Validation

Re-read ALL the same files again from disk (fresh read, do
not rely on memory from previous passes), then:

1. **Add or improve tests**:
   - Identify the existing test framework and patterns used
     in the project.
   - Add unit tests for individual functions and methods
     that were changed or created.
   - Add integration tests where components interact with
     each other.
   - Add e2e tests if the project already has an e2e test
     setup and the changes affect user-facing flows.
   - Follow the existing test conventions (naming,
     structure, assertions).
   - Ensure tests cover both happy paths and edge cases.

2. **Final coherence check**: With tests written, re-examine
   whether the code under test reveals any remaining
   incoherence, naming mismatch, or logic flaw. Fix
   anything found.

3. **Run tests**: Execute the test suite to confirm
   everything passes.

Apply fixes, then summarize what was changed in Pass 3.

---

## Final Summary

Present a consolidated report:

- **Pass 1** changes (structure & clean code)
- **Pass 2** changes (coherence & consistency)
- **Pass 3** changes (tests & validation)
- Total number of issues found and fixed across all passes

Do NOT change external behavior or add new features. This is
a pure refactoring and test coverage pass.
