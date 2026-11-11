# Tidy — a Super Voice post-processor

[![coverage](https://img.shields.io/endpoint?url=https://super-libre.github.io/super-voice-tidy/coverage.json)](https://super-libre.github.io/super-voice-tidy/)

Tidies up dictated text before it is typed. It takes the transcript your speech
model produced and cleans it up on the way to the cursor:

```
um so i i think that uh we should  ship it , today .
                          ↓
So I think that we should ship it, today.
```

This is a **post-processor** backend for
[Super Voice](https://github.com/jorge-menjivar/super-voice): the daemon hands it a
finished transcript on `POST /v1/process` and it hands back a cleaned one. It
does not transcribe audio.

## Why rules and not a model

The rules are the same family of normalization applied to text corpora before
training: fold typographic Unicode to ASCII, strip invisibles, drop vocalized
fillers, repair spacing, restore casing. That makes this backend

- **fast** — microseconds, not a model round trip, so dictation stays instant;
- **private** — no network egress at all (`allowed_hosts = []`), so your
  transcripts never leave the machine;
- **small** — a ~200 KB WebAssembly component, no weights to download;
- **predictable** — every rule is a rule you can read, and the same input always
  produces the same output.

An LLM post-processor can do more (rephrasing, formatting to intent). This one
does less, deliberately, and never invents words you did not say.

## What it does

Each rule is a separate setting, on by default, switchable in
**Settings → Models → Post-processing → Configure**.

| Rule | Setting | What it does |
|---|---|---|
| Normalize punctuation | `normalize_unicode` | Smart quotes → `'` `"`, en/em dashes → `-`, `…` → `...`, non-breaking spaces → spaces; zero-width characters and control bytes dropped. |
| Remove filler sounds | `remove_fillers` | Drops `um`, `uh`, `erm`, `hmm` and similar. Sentence-ending punctuation on a dropped filler moves back onto the previous word. |
| Collapse stutters | `dedupe_words` | `the the` → `the`. |
| Repair punctuation spacing | `fix_punctuation` | No space before `,` `.` `!` `?`, one space after; doubled marks collapsed; brackets unpadded. |
| Collapse whitespace | `collapse_whitespace` | Whitespace runs → one space, ends trimmed. |
| Fix sentence casing | `sentence_case` | Capitalizes sentence starts and the standalone pronoun `i`. |

## What it deliberately does not do

These are the cases where a cleanup rule would change your meaning, so it
doesn't run:

- **Only sounds are treated as fillers.** `like`, `so`, `you know` and `I mean`
  are fillers in speech and ordinary words in sentences, and nothing here can
  tell which one you meant.
- **Grammatical doubles survive.** `had had`, `that that`, `is is`, `was was`
  and `no no` are exempt from stutter collapsing.
- **Interior casing is left alone.** `NASA`, `iPhone` and `i.e.` are the
  transcription model's output, not this backend's to rewrite. Only the first
  letter of a sentence is touched, and a `.` only ends a sentence when
  whitespace follows it — so `3.5` and `e.g.` stay intact.
- **Nothing is added.** No rephrasing, no invented punctuation beyond spacing
  repair, no words you did not say.

Cleaning is **idempotent**: running an already-clean transcript through again
changes nothing, so a transcript never degrades by passing through twice.

If every rule removes everything — a transcript of nothing but `um uh hmm` —
the result is empty, and the daemon keeps your raw text instead.

## Install

**From the registry:** Settings → Library → Browse → *Tidy* → Install.
Then Models → Post-processing, select it as the backend, pick its model and
press **Load model**.

**From this directory:**

```bash
rustup target add wasm32-wasip2
just stage        # builds and writes tidy.wasm next to backend.toml
```

Then Settings → Library → Installed → *Import from dir*, and point it here.

## Develop

```bash
just test         # unit tests (the rules are pure and host-testable)
just ci           # fmt + lint (component and host) + build + test
```

The cleanup rules live in [`src/clean.rs`](src/clean.rs) and compile for the
host, so they are tested natively. [`src/lib.rs`](src/lib.rs) is the
`wasi:http` component shell that implements the `/v1` contract; it compiles
only for `wasm32`.

Adding a rule means: a function plus its tests in `clean.rs`, a field on
`Rules`, a line in `rules_from_options`, and an `[[options]]` entry in
`backend.toml`.

## Protocol

Implements the [Super Voice backend contract](https://github.com/jorge-menjivar/super-voice/blob/main/docs/protocol/backend/contract.md)
for the post-processor role:

| Route | Behavior |
|---|---|
| `GET /v1/ping` | `pong`. |
| `GET /v1/status` | Always `ready` — there are no weights to load. |
| `POST /v1/load` | Accepted no-op. |
| `POST /v1/process` | Cleans the submitted `text`; `400 invalid_text` when it is missing or blank. |
| `POST /v1/cancel` | `409 nothing_in_progress` — cleanup is synchronous. |
| `POST /v1/transcribe` | `400 unsupported_role` — this backend does not transcribe. |

## License

GPL-3.0-only. See [LICENSE](LICENSE).
