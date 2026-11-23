// SPDX-License-Identifier: Apache-2.0

//! A limit or setting the server cannot use stops it before it binds.
//!
//! `GRPC_PDF_MAX_BYTES=512M` used to start the server on the 128 MiB
//! default without a word, so the operator believed a limit was in force
//! that was not. Every limit and setting is now refused the way
//! `GRPC_PDF_MAX_UPLOAD_SECONDS` always was.

use std::process::Command;

/// Run the server binary with one extra variable and return its exit code
/// and standard error. It never gets as far as binding: an unusable value
/// stops it first, and the listen address is unroutable in case it does.
fn start_with(name: &str, value: &str) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_grpc-pdf-inspector"))
        .env("GRPC_PDF_ADDR", "203.0.113.1:1")
        .env(name, value)
        .output()
        .expect("run the server binary");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn an_unusable_value_stops_the_server_and_names_the_variable() {
    for (name, value) in [
        ("GRPC_PDF_MAX_BYTES", "512M"),
        ("GRPC_PDF_MAX_PARSE_SECONDS", "10m"),
        ("GRPC_PDF_MAX_CONCURRENT_PARSES", "0"),
        ("GRPC_PDF_WORKERS", "all"),
        ("GRPC_PDF_WINDOW_BYTES", "4M"),
        ("GRPC_PDF_LOG", "loud"),
    ] {
        let (code, stderr) = start_with(name, value);
        assert_eq!(code, Some(2), "{name}={value}: {stderr}");
        assert!(
            stderr.contains("refusing to start") && stderr.contains(name),
            "{name}={value}: {stderr}"
        );
    }
}
