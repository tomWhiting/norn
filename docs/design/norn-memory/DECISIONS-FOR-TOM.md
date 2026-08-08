# Memory design — what needs your say-so

**For:** Tom
**From:** Sable
**Date:** 2026-08-08

Everything on this feature is waiting on you. The decisions were spread across
two long technical documents, so this is all of them in one place, in plain
terms, with what I'd do and what happens if you say nothing.

**You don't need to do these in order, and none of them is urgent.** The first
one is the only one that blocks the others.

---

## Settled by you today (8 Aug, after the compaction) — folded in, nothing to do

- **Lanterns are declared, not automatic.** An agent lights one on purpose, at
  a moment of completion, success, or learning. Fork finishes still leave
  their automatic record, but that record isn't a lantern by itself.
- **Notes grow over time.** A note is never rewritten — later entries get
  added to it, so you can see what wound up happening. That trail is also how
  ancestors' mistakes become learnable.
- **Vectors are in**, for the one job only they can do: finding the ancestor
  who fought the same *kind* of problem in a *different place*. Guards: exact
  matches always rank first, offers show titles only, and we record whether
  agents actually use them.

These added **two new questions: 9 and 10 below.**

---

## 1. Where the truth lives — the only one that blocks anything

**The question:** when an agent records something, is the permanent record the
**log** (a simple file that only ever gets added to), with everything else
rebuilt from it? Or is a **database** the permanent record?

**Why it matters to you:** an earlier spec said the database. Everything I've
worked out since points the other way, and if we go with the log, several
problems disappear rather than get solved:

- no database contention between agents
- nothing to corrupt, because rebuilding is always available
- no background service needed
- no new database at all for the first version

**I'd say: the log.** But this contradicts something already written down, so
it's yours to change rather than mine to quietly edit.

**If you say nothing:** everything below stays parked, because it all assumes
an answer to this.

---

## 2. Fading

**The question:** do you confirm that memories fade based on **how much the
code has changed**, never on how much time has passed?

**This is your own steer** — I'm asking you to confirm it as a rule because it
has one consequence you didn't say out loud:

**Notes must not fade at all.** A record of "work happened here" should dim when
the code moves on. A note saying "here's *why* we did it this way" shouldn't —
after a rewrite it's often the only trace of the reasoning left.

**I'd say: confirm both.** The second half is what makes the first half safe.

---

## 3. How memories get chosen

**The question:** do you accept matching in **bands** rather than a blended
score?

Show past work on this exact file first. Then work on files it depends on. Then
work that sounds like what you're doing. Each band used up before the next.

**Why not a blend:** "half this, a third that" would be three numbers I invented
and nothing could justify. Bands need none, and every "why did this show up?"
has an exact answer.

**Also in this one:** record whether agents actually *use* what surfaces. It's
the only way to tell memory that helps from memory that just costs money, and
without it the failure is invisible.

**I'd say: yes to both.**

---

## 4. How much room memory gets

**The question:** what share of an agent's reading space may memory take up?

**This is a number and it has to be yours.** I've tied it to something real
rather than picking a count of items, but I won't invent the share itself.

**If you say nothing:** the feature can't ship, because the alternative is me
guessing.

---

## 5. Similarity search — you've ruled it in; here's what that means

**You said today vectors have value, and I agree on one specific job:** they're
the only way to find the ancestor who fought the same kind of problem
somewhere else — different file, different names. That's usually where the
mistake you're about to repeat lives.

**The good news:** at our size this needs **no new database at all**. A few
thousand notes — plain arithmetic over a small file does it exactly. The
haematite question I'd flagged before doesn't come up.

**Ordinary word matching still goes in first**, and we measure both. If the
word matching turns out to catch everything, the numbers will show it.

**What's left of this one is question 9 below.**

---

## 6. Anything planned as a background service

**The question:** is there a plan somewhere that assumed memory needs a
background service running?

**If so it should be re-examined**, because the reason for it was never true —
I'd assumed the memory index needed to be *told* when things changed. It
doesn't.

**I'd say: worth a look, low priority.**

---

## 7. Agent forks and their notes

**The question, still open from July:** when an agent branches off to do a task
and comes back — do notes it made along the way come back with it?

**I've not written this up properly yet** because you asked me to hold. Say the
word and I will.

---

## 8. The branches

Six branches are pushed and waiting: two on this design, one hardening an
earlier spec, and three that are documentation only.

**None of them changes any code.** They're all writing.

**I'd say: land the three documentation ones whenever convenient, and hold the
design ones until you've ruled above** — so what lands matches what you decided.

---

## 9. Which model turns text into vectors

**The question:** the vector search needs a model to do the comparing. Which
one — something running on your machines, or a paid service?

**Why it matters to you:** it's a cost, and it decides whether agents'
work-notes ever leave the machine. Both are your calls, not mine.

**If you say nothing:** the word-matching version ships and the vector part
waits with this question.

---

## 10. Telling agents when to light a lantern

**The question:** for agents to declare lanterns at the right moments, we have
to change the standing instructions every agent gets. May I?

**Why it matters to you:** it changes what every agent writes down, across the
whole estate, from that day on. It's a one-time switch and it should be thrown
by you, on purpose — not slipped in by me.

**If you say nothing:** nothing changes, and no lanterns get lit.

---

## Not on this list, because it isn't yours

The aion fault, the haematite tickets, and the sequence-number work are all
owned by Vesper and Apollo. **The only thing outstanding from you there is
whether Apollo may start writing the fix** — they're holding for your word.
