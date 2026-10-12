use std::path::Path;
use std::process::{Command, Stdio};

use crate::diagnostic::{Diagnostic, Result, Span};
use crate::limits::{Limit, MAX_TEXT_BYTES};

pub(super) fn run(command: &[String], folder: Option<&Path>, span: Span) -> Result<String> {
    let Some((program, arguments)) = command.split_first() else {
        return Err(Diagnostic::new(
            "E-PROC-001",
            span,
            "실행할 프로그램 이름이 없습니다.",
        ));
    };
    let mut process = Command::new(program);
    process.args(arguments).stdin(Stdio::null());
    if let Some(folder) = folder {
        process.current_dir(folder);
    }
    let output = process.output().map_err(|error| {
        Diagnostic::new(
            "E-PROC-001",
            span,
            format!("'{program}' 프로그램을 실행하지 못했습니다: {error}"),
        )
    })?;
    if !output.status.success() {
        let code = output
            .status
            .code()
            .map_or_else(|| "없음".to_owned(), |code| code.to_string());
        let error = String::from_utf8_lossy(&output.stderr);
        let error: String = error.trim().chars().take(1000).collect();
        return Err(Diagnostic::new(
            "E-PROC-002",
            span,
            if error.is_empty() {
                format!("'{program}' 프로그램이 실패했습니다(종료 코드 {code}).")
            } else {
                format!("'{program}' 프로그램이 실패했습니다(종료 코드 {code}): {error}")
            },
        ));
    }
    if output.stdout.len() > MAX_TEXT_BYTES {
        return Err(Diagnostic::limit(Limit::Text, span));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim_end_matches(['\n', '\r']).to_owned())
}

pub(crate) fn environment() -> Vec<(String, String)> {
    let mut variables: Vec<_> = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let name = if cfg!(windows) {
                name.to_uppercase()
            } else {
                name
            };
            Some((name, value.into_string().ok()?))
        })
        .collect();
    variables.sort();
    variables
}
