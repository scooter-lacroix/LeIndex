# General Coding Guide

These are the defaults I follow in any codebase, regardless of language or framework.

These rules are mandatory defaults for new code. I only break them when a project constraint is real, documented, and local.

## What I optimize for

- Make the next change easy, not just the current change fast.
- Prefer obvious code over clever code; I only pay complexity when it clearly buys something.
- Push invariants into types, schemas, and structure so fewer things rely on memory and discipline.
- Keep behavior local: the fewer files I need to understand to change something, the better.
- Treat performance, reliability, and security as design constraints, not cleanup work.

## Design defaults

- Start with the smallest design that can survive likely change; add abstraction only after I see repetition or pressure.
- Separate pure logic from IO so business rules stay testable and side effects stay explicit.
- Model domain concepts directly; avoid generic `utils`, `helpers`, and catch-all service layers.
- Validate and normalize data at boundaries, then keep internal code working with trusted shapes.
- Delete dead code and unused configuration instead of preserving speculative flexibility.

## Naming and structure

- Use names that reveal role and scope: `buildInvoice`, `InvoiceRepository`, `retryWindow`, not vague verbs or nouns.
- Keep files and modules focused; if a file has unrelated reasons to change, I split it.
- Put related code together so readers can follow one feature without jumping around the project.
- Prefer consistent local patterns over importing a new pattern for every file.
- Write comments for intent, trade-offs, or non-obvious constraints; the code should handle the 'what'.

## Testing and quality

- Test the most important behavior at seams: parsing, domain rules, persistence, network edges, and failure paths.
- Use fast unit tests for logic and targeted integration tests for boundaries; avoid giant brittle end-to-end pyramids.
- Reproduce bugs with tests when practical before fixing them.
- Let formatters and linters enforce style so review time goes to logic, risk, and design.
- When editing existing code, I respect the local conventions unless they actively harm clarity or correctness.

## Explicitly prohibited

The following practices are prohibited in new code unless the guide names a narrow, explicit exception.

- Hidden control flow, magic state, and action at a distance.
- Boolean flag piles that should be enums or distinct types.
- Swallowing errors, retrying blindly, or returning ambiguous success values.
- Over-abstracted code that exists to look reusable rather than solve a present problem.
- Large rewrites when a focused, verifiable change will do.
