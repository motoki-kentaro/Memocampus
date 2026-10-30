//! Launches that are not a double-click never show the guide: the host runs its Native
//! Messaging loop and writes nothing but protocol frames to stdout.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs the host with `stdin`, sending `input` if it is a pipe. A host stuck in a modal dialog
/// would never exit, so it is killed after a while and the test fails.
fn run(stdin: Stdio, input: &[u8], args: &[&str]) -> Output {
    let app = std::env::temp_dir().join(format!("memocampus-launch-{}", uuid::Uuid::new_v4()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_memocampus-native-host"))
        .args(args)
        .env("MEMOCAMPUS_APP_DIR", &app)
        .env_remove("MEMOCAMPUS_DB")
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(input).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("the host did not exit (a dialog would block it)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(app);
    output
}

#[test]
fn an_empty_pipe_ends_the_protocol_loop_silently() {
    let output = run(Stdio::piped(), b"", &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "no guide text on stdout");
}

#[test]
fn nul_as_stdin_is_not_mistaken_for_a_double_click() {
    let output = run(Stdio::null(), b"", &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
}

#[test]
fn a_chrome_style_launch_answers_with_exactly_one_protocol_frame() {
    let body = br#"{"type":"no_such_type","id":"e1"}"#;
    let mut input = (body.len() as u32).to_ne_bytes().to_vec();
    input.extend(body);
    let args = [
        "chrome-extension://abcdefghijklmnopabcdefghijklmnop/",
        "--parent-window=0",
    ];
    let output = run(Stdio::piped(), &input, &args);
    assert!(output.status.success(), "{output:?}");
    let length = u32::from_ne_bytes(output.stdout[..4].try_into().unwrap()) as usize;
    assert_eq!(
        output.stdout.len(),
        4 + length,
        "nothing before or after the frame"
    );
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout[4..]).unwrap();
    assert_eq!(
        reply,
        serde_json::json!({ "ok": false, "eventId": "e1", "error": "unknown_type" })
    );
}
