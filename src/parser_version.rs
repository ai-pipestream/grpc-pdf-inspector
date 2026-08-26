// SPDX-License-Identifier: Apache-2.0

//! The parser attribution string, derived from the manifest at compile
//! time.
//!
//! `CollectorSource.model` exists so a downstream merge can tell which
//! engine produced an item. That makes a wrong version worse than no
//! version, and a hand-maintained literal is a wrong version waiting to
//! happen: this crate carried `"pdf-inspector 1.15"` for a release after
//! the dependency moved to 1.17.0, and nothing failed.
//!
//! Cargo publishes no environment variable for a dependency's version, and
//! this crate deliberately has no build script (see `AGENTS.md`: codegen is
//! dev-time, the image has no protoc and no build tooling). What is left is
//! the manifest itself, which `include_str!` makes available to `const`
//! evaluation. Everything below therefore runs at compile time, allocates
//! nothing, and fails the build rather than the parse when the line it
//! expects is not there.
//!
//! The manifest it reads is the *vendored crate's* own, because that is
//! where the version of the linked parser is now stated: this service
//! depends on `vendor/pdf-inspector` by path, and a path dependency carries
//! no version in the depending manifest to read.

/// The name of the parser crate this build links.
pub const PARSER_CRATE: &str = "pdf-inspector";

/// The vendored parser's own manifest, the source of truth for the version
/// below.
const MANIFEST: &str = include_str!("../vendor/pdf-inspector/Cargo.toml");

/// Version of the parser this build links, read out of the vendored crate's
/// manifest.
///
/// It is the released version the vendored copy was taken from. The patches
/// applied on top of it (see `vendor/pdf-inspector/README.md`) are additive
/// visibility changes: they make the crate return data it already computed
/// and change nothing it parses, so the version still names the parser's
/// behaviour honestly.
pub const PARSER_VERSION: &str = manifest_value(MANIFEST, "version");

/// The vendored manifest describes the crate this build claims to link.
///
/// A copy of some other crate landing in `vendor/pdf-inspector` would
/// otherwise be reported under this crate's name; this fails the build
/// instead.
const _: () = assert!(
    equal(manifest_value(MANIFEST, "name"), PARSER_CRATE),
    "vendor/pdf-inspector holds a crate other than pdf-inspector"
);

/// Name and version of the parser this build links, attached to every
/// Document item's `CollectorSource.model`.
pub const PARSER: &str = joined();

/// Find `\n<key> = "` in `manifest` and return the string literal that
/// follows it.
///
/// A `const fn`, so a manifest that does not state the key the way this
/// expects (a renamed key, a table-form `{ version = ... }`, a caret range)
/// is a compile error naming the problem, not a runtime surprise.
const fn manifest_value<'a>(manifest: &'a str, key: &str) -> &'a str {
    let manifest = manifest.as_bytes();
    let start = match declaration_end(manifest, key.as_bytes()) {
        Some(start) => start,
        None => panic!(
            "the vendored parser's Cargo.toml no longer states `<key> = \"<value>\"`; \
             update src/parser_version.rs to match how it is stated now"
        ),
    };
    let mut end = start;
    while end < manifest.len() && manifest[end] != b'"' {
        // A range requirement would make the version this reports a
        // requirement rather than a fact, which is the bug this module
        // exists to prevent.
        assert!(
            !matches!(
                manifest[end],
                b'^' | b'~' | b'*' | b'=' | b'<' | b'>' | b','
            ),
            "the parser's version must be an exact one so the model string \
             names what is actually linked"
        );
        end += 1;
    }
    assert!(end < manifest.len(), "unterminated version string");
    slice(manifest, start, end)
}

/// Byte offset just past `\n<name> = "` in `manifest`, when it occurs.
///
/// Anchoring on the newline keeps the match on a key rather than on the
/// same name inside a comment or a longer key.
const fn declaration_end(manifest: &[u8], name: &[u8]) -> Option<usize> {
    let mut at = 0;
    while at < manifest.len() {
        if manifest[at] == b'\n' && starts_with(manifest, at + 1, name) {
            let after = at + 1 + name.len();
            if starts_with(manifest, after, b" = \"") {
                return Some(after + 4);
            }
        }
        at += 1;
    }
    None
}

/// Whether `needle` sits at `at` in `haystack`.
const fn starts_with(haystack: &[u8], at: usize, needle: &[u8]) -> bool {
    if at + needle.len() > haystack.len() {
        return false;
    }
    let mut index = 0;
    while index < needle.len() {
        if haystack[at + index] != needle[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Whether two strings hold the same bytes.
///
/// `str`'s own `==` is not usable in `const` evaluation.
const fn equal(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut at = 0;
    while at < left.len() {
        if left[at] != right[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// `bytes[start..end]` as a string.
const fn slice(bytes: &[u8], start: usize, end: usize) -> &str {
    let (_, rest) = bytes.split_at(start);
    let (wanted, _) = rest.split_at(end - start);
    match core::str::from_utf8(wanted) {
        Ok(text) => text,
        Err(_) => panic!("a version literal is ASCII"),
    }
}

/// Length of `PARSER`: the crate name, a space, and the version.
const PARSER_LEN: usize = PARSER_CRATE.len() + 1 + PARSER_VERSION.len();

/// `PARSER_CRATE`, a space, and `PARSER_VERSION`, assembled at compile
/// time.
///
/// `concat!` takes literals only and the standard library has no `const`
/// string join, so the bytes are copied into a buffer whose length is
/// itself a `const` expression.
const fn joined() -> &'static str {
    const BYTES: [u8; PARSER_LEN] = {
        let mut out = [0u8; PARSER_LEN];
        let name = PARSER_CRATE.as_bytes();
        let mut at = 0;
        while at < name.len() {
            out[at] = name[at];
            at += 1;
        }
        out[at] = b' ';
        at += 1;
        let version = PARSER_VERSION.as_bytes();
        let mut index = 0;
        while index < version.len() {
            out[at + index] = version[index];
            index += 1;
        }
        out
    };
    match core::str::from_utf8(&BYTES) {
        Ok(text) => text,
        Err(_) => panic!("a crate name and a version are ASCII"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parser_version_is_the_one_the_vendored_manifest_states() {
        let declared = MANIFEST
            .lines()
            .find_map(|line| line.strip_prefix("version = \""))
            .and_then(|rest| rest.split('"').next())
            .expect("the vendored manifest states the crate's version");
        assert_eq!(PARSER_VERSION, declared);
    }

    #[test]
    fn the_vendored_crate_is_the_parser_this_service_names() {
        assert_eq!(manifest_value(MANIFEST, "name"), PARSER_CRATE);
    }

    #[test]
    fn the_model_string_is_the_crate_and_its_version() {
        assert_eq!(PARSER, format!("{PARSER_CRATE} {PARSER_VERSION}"));
        assert!(
            !PARSER.contains("1.15"),
            "the drifted literal is gone: {PARSER}"
        );
    }

    #[test]
    fn a_declaration_that_is_not_an_exact_pin_is_found() {
        // The reader anchors on `\n<key> = "`, so a table-form or renamed
        // declaration is not silently read as something else.
        assert_eq!(manifest_value("\nthing = \"2.0.1\"\n", "thing"), "2.0.1");
        assert_eq!(
            declaration_end(b"\nthing = { version = \"2\" }", b"thing"),
            None
        );
        assert_eq!(declaration_end(b"# thing = \"2.0.1\"", b"thing"), None);
    }
}
