// SPDX-License-Identifier: GPL-3.0-only
//! Transcript cleanup backend as a `wasi:http` proxy component.
//!
//! Implements the Super Voice `/v1` backend contract
//! (`docs/protocol/backend/contract.md`) for the **post-processor** role: the
//! daemon hands it a finished transcript on `POST /v1/process` and it hands
//! back a tidied one.
//!
//! Everything it does is deterministic and local — see [`clean`]. There are no
//! weights to download, no network egress, and no secrets: the component is a
//! few hundred lines of string handling in a sandbox with nothing granted to
//! it.
//!
//! The `wasi:http` handler compiles only for `wasm32`; the rules themselves
//! build for the host too, so they are unit-tested natively while the component
//! as a whole is driven over the real contract.

pub mod clean;

pub use clean::{Rules, clean};

/// The model name this backend serves, as declared in `backend.toml`.
///
/// Versioned like every other backend's models (`nova-3`, `whisper-1`): the
/// rules *are* the engine here, so a change that alters output for the same
/// input gets `rules-v2` rather than silently rewriting what `rules-v1` meant.
pub const MODEL: &str = "rules-v1";

/// Read a boolean `[[options]]` value.
///
/// Option values arrive as the strings the settings UI stored, so anything the
/// user could reasonably have typed for "off" is honored; an unset or
/// unparseable value falls back to `default`, which keeps a typo from silently
/// disabling a rule the manifest says is on.
#[must_use]
pub fn flag(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim) {
        Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        _ => default,
    }
}

/// Build the [`Rules`] a request runs under from its option headers.
///
/// `lookup` resolves an option name to its value, so this is testable without a
/// wasi request in hand.
#[must_use]
pub fn rules_from_options(lookup: impl Fn(&str) -> Option<String>) -> Rules {
    let d = Rules::default();
    let read = |name: &str, default: bool| flag(lookup(name).as_deref(), default);
    Rules {
        normalize_unicode: read("normalize_unicode", d.normalize_unicode),
        remove_fillers: read("remove_fillers", d.remove_fillers),
        dedupe_words: read("dedupe_words", d.dedupe_words),
        fix_punctuation: read("fix_punctuation", d.fix_punctuation),
        collapse_whitespace: read("collapse_whitespace", d.collapse_whitespace),
        sentence_case: read("sentence_case", d.sentence_case),
    }
}

/// Extract the `text` field from a `POST /v1/process` body.
///
/// Returns `Err` with the contract's `invalid_text` identifier when the body is
/// not JSON, carries no `text`, or carries an empty one.
///
/// # Errors
/// Returns `invalid_text` for a missing, non-string, or blank `text`.
pub fn parse_process_body(body: &[u8]) -> Result<String, &'static str> {
    let json: serde_json::Value = serde_json::from_slice(body).map_err(|_| "invalid_text")?;
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("invalid_text");
    }
    Ok(text.to_string())
}

/// Look up a request header by name, ignoring case.
///
/// Ask for the `x-voice-…` name. A header that is absent under it is looked
/// up under its Super STT name, `x-stt-…`: a Super Voice daemon sends every
/// header under both, and a Super STT daemon only under the old one.
#[must_use]
pub fn header(entries: &[(String, Vec<u8>)], want: &str) -> Option<String> {
    let find = |name: &str| {
        entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
    };
    find(want).or_else(|| {
        let rest = want.strip_prefix("x-voice-")?;
        find(&format!("x-stt-{rest}"))
    })
}

#[cfg(target_arch = "wasm32")]
mod component {
    use super::{MODEL, clean, header, parse_process_body, rules_from_options};

    use wasi::exports::http::incoming_handler::Guest;
    use wasi::http::types::{
        Fields, IncomingBody, IncomingRequest, Method, OutgoingBody, OutgoingResponse,
        ResponseOutparam,
    };
    use wasi::io::streams::StreamError;

    struct Component;

    impl Guest for Component {
        fn handle(request: IncomingRequest, outparam: ResponseOutparam) {
            let (status, body) = route(&request);
            send_response(outparam, status, &body);
        }
    }

    wasi::http::proxy::export!(Component);

    /// Dispatch a `/v1` request to its handler, returning `(status, json_bytes)`.
    fn route(request: &IncomingRequest) -> (u16, Vec<u8>) {
        let method = request.method();
        let full = request.path_with_query().unwrap_or_default();
        let path = full.split('?').next().unwrap_or("").to_string();

        match (&method, path.as_str()) {
            (Method::Get, "/v1/ping") => ok(&serde_json::json!({
                "status": "success", "message": "pong"
            })),
            // Nothing to load: there are no weights. The contract requires the
            // route to exist and requires `ready` immediately, which is exactly
            // what a backend with nothing to load reports.
            (Method::Get, "/v1/status") => ok(&serde_json::json!({
                "status": "success",
                "state": "ready",
                "device": "cpu",
                "model": { "name": MODEL },
            })),
            (Method::Post, "/v1/load") => (
                202,
                to_vec(&serde_json::json!({ "status": "success", "message": "Loading started" })),
            ),
            // Cleanup is synchronous and near-instant, so nothing is ever in
            // flight to cancel.
            (Method::Post, "/v1/cancel") => (
                409,
                to_vec(&serde_json::json!({
                    "status": "error", "message": "nothing_in_progress"
                })),
            ),
            (Method::Post, "/v1/process") => process(request),
            // This backend serves the post-processor role only. Saying so is
            // more useful than a bare 404 if a daemon ever routes audio here.
            (Method::Post, "/v1/transcribe") => (
                400,
                to_vec(&serde_json::json!({
                    "status": "error",
                    "message": "unsupported_role",
                    "detail": "This backend post-processes transcripts; it does not transcribe audio.",
                })),
            ),
            _ => err(404, "not_found"),
        }
    }

    /// Handle `POST /v1/process`: read the rule options off the injected
    /// headers, clean the submitted transcript, and return it.
    fn process(request: &IncomingRequest) -> (u16, Vec<u8>) {
        let entries = request.headers().entries();
        let rules = rules_from_options(|name| header(&entries, &format!("x-voice-option-{name}")));

        let Ok(incoming) = request.consume() else {
            return err(400, "invalid_text");
        };
        let Ok(body) = read_all(incoming) else {
            return err(400, "invalid_text");
        };
        let text = match parse_process_body(&body) {
            Ok(text) => text,
            Err(message) => return err(400, message),
        };

        ok(&serde_json::json!({
            "status": "success",
            "text": clean(&text, &rules),
        }))
    }

    fn ok(value: &serde_json::Value) -> (u16, Vec<u8>) {
        (200, to_vec(value))
    }

    fn err(status: u16, message: &str) -> (u16, Vec<u8>) {
        (
            status,
            to_vec(&serde_json::json!({ "status": "error", "message": message })),
        )
    }

    fn to_vec(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap_or_default()
    }

    /// Drain an incoming body to bytes.
    fn read_all(body: IncomingBody) -> Result<Vec<u8>, String> {
        let stream = body.stream().map_err(|()| "no_stream".to_string())?;
        let mut out = Vec::new();
        loop {
            match stream.blocking_read(65536) {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(StreamError::LastOperationFailed(_)) => return Err("read_failed".to_string()),
            }
        }
        drop(stream);
        let _ = IncomingBody::finish(body);
        Ok(out)
    }

    /// Build the response and hand it to the outparam.
    fn send_response(outparam: ResponseOutparam, status: u16, body_bytes: &[u8]) {
        let headers = Fields::new();
        let _ = headers.append("content-type", b"application/json");
        let response = OutgoingResponse::new(headers);
        let _ = response.set_status_code(status);
        let Ok(body) = response.body() else {
            ResponseOutparam::set(outparam, Ok(response));
            return;
        };
        ResponseOutparam::set(outparam, Ok(response));
        if let Ok(stream) = body.write() {
            for chunk in body_bytes.chunks(4096) {
                let _ = stream.blocking_write_and_flush(chunk);
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(body, None);
    }
}

#[cfg(test)]
mod tests {
    use super::{Rules, flag, header, parse_process_body, rules_from_options};

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn flag_reads_the_spellings_a_settings_field_can_hold() {
        for on in ["true", "1", "yes", "on", " true "] {
            assert!(flag(Some(on), false), "{on:?} should read as on");
        }
        for off in ["false", "0", "no", "off"] {
            assert!(!flag(Some(off), true), "{off:?} should read as off");
        }
    }

    /// An unset option keeps the manifest's default, and so does a value
    /// nobody can parse — a typo must not silently disable a rule.
    #[test]
    fn an_unset_or_unparseable_flag_keeps_its_default() {
        assert!(flag(None, true));
        assert!(!flag(None, false));
        assert!(flag(Some("perhaps"), true));
        assert!(!flag(Some(""), false));
    }

    #[test]
    fn options_with_nothing_set_yield_the_defaults() {
        assert_eq!(rules_from_options(|_| None), Rules::default());
    }

    #[test]
    fn an_option_switches_exactly_its_own_rule() {
        let rules =
            rules_from_options(|name| (name == "remove_fillers").then(|| "false".to_string()));
        assert_eq!(
            rules,
            Rules {
                remove_fillers: false,
                ..Rules::default()
            }
        );
    }

    #[test]
    fn a_process_body_yields_its_text() {
        assert_eq!(
            parse_process_body(br#"{"text":"hello there","language":"en"}"#).unwrap(),
            "hello there"
        );
    }

    /// The contract's `invalid_text`: the daemon never sends an empty
    /// transcript, so anything blank here is a malformed request.
    #[test]
    fn a_missing_or_blank_text_is_invalid() {
        for body in [
            &b"{}"[..],
            &br#"{"text":""}"#[..],
            &br#"{"text":"   "}"#[..],
            &br#"{"text":42}"#[..],
            &b"not json"[..],
        ] {
            assert_eq!(parse_process_body(body), Err("invalid_text"));
        }
    }

    #[test]
    fn an_option_is_read_under_its_super_voice_name() {
        let entries = headers(&[("X-Voice-Option-remove_fillers", "false")]);
        assert_eq!(
            header(&entries, "x-voice-option-remove_fillers").as_deref(),
            Some("false")
        );
    }

    /// A Super STT daemon sends only the `x-stt-` names.
    #[test]
    fn an_option_sent_only_under_its_super_stt_name_is_still_read() {
        let entries = headers(&[("x-stt-option-remove_fillers", "false")]);
        assert_eq!(
            header(&entries, "x-voice-option-remove_fillers").as_deref(),
            Some("false")
        );
    }

    #[test]
    fn the_super_voice_name_wins_when_both_are_sent() {
        let entries = headers(&[
            ("x-stt-option-sentence_case", "true"),
            ("x-voice-option-sentence_case", "false"),
        ]);
        assert_eq!(
            header(&entries, "x-voice-option-sentence_case").as_deref(),
            Some("false")
        );
    }

    #[test]
    fn an_unsent_option_is_none() {
        let entries = headers(&[("x-voice-option-dedupe_words", "true")]);
        assert_eq!(header(&entries, "x-voice-option-remove_fillers"), None);
        assert_eq!(header(&entries, "x-voice-model"), None);
    }
}
