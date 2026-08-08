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

## 5. Similarity search — my advice is to wait

**The question:** do we wait for haematite to grow proper search, or build our
own now?

**I'd say: wait.** Building our own means building the thing haematite's own
design says should live inside it. And there's a middle option that needs
neither — ordinary word matching, which may actually work better for our kind
of text.

**If you say nothing:** we wait. This one is safe to leave.

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

## Not on this list, because it isn't yours

The aion fault, the haematite tickets, and the sequence-number work are all
owned by Vesper and Apollo. **The only thing outstanding from you there is
whether Apollo may start writing the fix** — they're holding for your word.
