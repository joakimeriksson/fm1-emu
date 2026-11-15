# Interview: what to ask the human, and how little

The audit tells you what exists. The interview tells you what the code structurally cannot: intent, audience, "why," tone, scope boundaries, near-term roadmap. Without it, the docs — README above all — come out accurate but generic and miss the point of the project.

This is a setup step, not an interrogation. Proportion is the whole game.

## Rules

1. **Narrow with the audit first.** Never ask what you already know. If the manifest says TypeScript + Next.js, don't ask the language or framework. Ask only what the audit *can't* answer.
2. **Batch, don't drip.** Ask everything in one structured pass (use the host's multi-question UI if available), so the human answers once. Don't trickle one question at a time.
3. **Cap at ~3–6 questions.** If you're tempted to ask more, you're probably asking things the audit covers, or things that don't change what you write.
4. **Specific, not open-ended.** "Who is the primary user — end users, internal devs, or other services?" beats "Tell me about your project." No essay prompts.
5. **Make answering optional.** If the human is unavailable or declines, proceed with audit facts but mark intent-dependent content (especially README) as "needs human input" instead of fabricating intent.
6. **Don't re-ask on reconcile.** The interview is a bootstrap step. On reconcile, only ask if something *material* changed that you genuinely can't infer.

## Question bank (pick the 3–6 that the audit left open)

**Intent / audience (almost always ask — README depends on it):**
- What problem does this solve, and who is it for? (end users / internal team / other services / library consumers)
- What's the one thing a newcomer most needs to understand that isn't obvious from the code?

**Positioning / tone:**
- How should the README read — minimal and utilitarian, or warm and welcoming? Any existing voice to match?
- What do you most want highlighted, and what's intentionally out of scope?

**Constraints the code doesn't state:**
- Any hard rules an agent must follow that aren't enforced in config? (things never to touch, deploy/release gotchas, security boundaries)
- Any near-term roadmap or in-flight rewrite worth flagging so docs don't describe something about to change?

**UI / design (only if a UI surface was detected, and only lightly):**
- Is there an existing brand/style direction (colors, type, spacing, component conventions) I should encode, or should I propose a sensible starting one?

## Weaving answers in

- README: blend interview answers (why/who/tone) with audit facts (install/usage/stack) into human-toned prose. The interview is what stops it reading like generated boilerplate.
- Foundation (`context/overview.md`): intent and non-obvious rules from the interview, stack/structure from the audit.
- Router: hard constraints/gotchas from the "constraints" answers.
- design-system.md: the brand/style answer seeds it (encode existing direction, or propose and label it as proposed).

Keep the synthesis honest — if an answer was vague, don't over-claim. If it wasn't given, leave the section thin and flagged rather than invented.
