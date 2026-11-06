//! Minimal RUSTC_WORKSPACE_WRAPPER for local coverage with shared dependency caches.
//! Cargo supplies the rustc executable followed by its literal arguments.
use std::ffi::{OsStr, OsString};
use std::process::Command;

fn instrument(arguments: &[OsString]) -> bool {
    if arguments.iter().any(|argument| {
        matches!(
            argument.to_str(),
            Some("-vV" | "-V" | "--version" | "-h" | "--help" | "--print")
        ) || argument
            .to_str()
            .is_some_and(|value| value.starts_with("--print="))
    }) {
        return false;
    }
    arguments
        .windows(2)
        .any(|pair| pair[0] == OsStr::new("--crate-name") && pair[1] == OsStr::new("peerlume"))
        || arguments
            .iter()
            .any(|argument| argument == OsStr::new("--crate-name=peerlume"))
}

fn forwarded(arguments: &[OsString]) -> Vec<OsString> {
    let mut result = arguments.to_vec();
    if instrument(arguments) {
        result.push(OsString::from("-C"));
        result.push(OsString::from("instrument-coverage"));
    }
    result
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let Some(rustc) = arguments.next() else {
        eprintln!("GUI coverage wrapper: Cargo did not supply rustc");
        std::process::exit(2);
    };
    let arguments: Vec<_> = arguments.collect();
    match Command::new(rustc).args(forwarded(&arguments)).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("GUI coverage wrapper could not launch rustc: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn only_application_library_gets_coverage_and_arguments_stay_literal() {
        let input = args(&[
            "--crate-name",
            "peerlume",
            "--edition=2024",
            "C:/space here/é.rs",
            "--test",
            "--cfg",
            "feature=\"gui\"",
            "-C",
            "debuginfo=0",
        ]);
        let actual = forwarded(&input);
        assert_eq!(&actual[..input.len()], input);
        assert_eq!(&actual[input.len()..], args(&["-C", "instrument-coverage"]));
        assert!(instrument(&args(&["--crate-name=peerlume", "src/lib.rs"])));
    }
    #[test]
    fn queries_dependencies_and_build_scripts_are_exact_passthrough() {
        for input in [
            args(&["-vV"]),
            args(&["--crate-name", "peerlume", "--print=file-names"]),
            args(&["--crate-name", "peerlume", "--print", "cfg"]),
            args(&["--crate-name", "eframe", "src/lib.rs"]),
            args(&["--crate-name", "build_script_build", "build.rs"]),
            args(&[
                "--crate-name",
                "peerlume_control",
                "src/bin/peerlume-control.rs",
            ]),
        ] {
            assert_eq!(forwarded(&input), input);
        }
    }
    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_not_lossily_reencoded() {
        use std::os::unix::ffi::OsStringExt;
        let mut input = args(&["--crate-name", "peerlume"]);
        input.push(OsString::from_vec(vec![b's', 0xff, b'.', b'r', b's']));
        assert_eq!(&forwarded(&input)[..input.len()], input);
    }
}
