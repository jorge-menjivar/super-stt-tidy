# Super Voice — Tidy backend

[![coverage](https://img.shields.io/endpoint?url=https://super-libre.github.io/super-voice-tidy/coverage.json)](https://super-libre.github.io/super-voice-tidy/)

Tidies up your dictation before it is typed, as a post-processor for
[Super Voice](https://github.com/jorge-menjivar/super-voice):

```
um so i i think that uh we should  ship it from my iphone , today .
                          ↓
So I think that we should ship it from my iPhone, today.
```

- **Clean text:** it removes filler sounds, collapses stutters, repairs
  punctuation and spacing, and fixes sentence casing. Each rule is its own
  setting.
- **Your vocabulary, your spelling:** terms from your dictation context, like
  `iPhone` or `GitHub`, come out the way you wrote them, and are never cut as a
  filler or a stutter.
- **Instant and private:** plain rules that run in microseconds, with no model
  to download and no network access, so your words never leave your machine.
  It never adds a word you didn't say.

## What it is

The Super Voice daemon does not compile model inference in-tree. It discovers
backends and drives each one over a `/v1` HTTP contract. This is one such
backend: a sandboxed WebAssembly component that answers `POST /v1/process` with
the cleaned transcript, with no network access at all.

## License

GPL-3.0-only. See `LICENSE`.
