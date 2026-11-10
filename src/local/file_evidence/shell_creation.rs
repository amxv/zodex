//! Conservative, command-derived creation evidence. This module never opens files.
//!
//! Plain redirection and shell noclobber are deliberately excluded: neither proves
//! exclusive creation for every destination type. Only a tiny isolated Python
//! program using exclusive creation and a literal UTF-8 write is recognized.
//! This is command-semantic evidence, not a later snapshot of the filesystem.

use std::path::Path;

use serde_json::Value;

use crate::invocation::InvocationStart;

use super::{
    CompletedFileEvidence, FileCapturePlan, FileEvidenceSource, FileOperationHint, FileSnapshot,
    MAX_FILE_EVIDENCE_BYTES, PendingFileEvidence,
};

pub(crate) fn completed_shell_creation(
    start: &InvocationStart,
    result: &Value,
) -> Option<(PendingFileEvidence, CompletedFileEvidence)> {
    if start.tool_name.as_ref() != "exec_command"
        || result.get("status")?.as_str()? != "exited"
        || result.get("exit_code")?.as_i64()? != 0
        || result
            .get("termination_reason")
            .is_some_and(|reason| reason != "exit")
    {
        return None;
    }
    let (plan, text) = parse_creation(&start.arguments)?;
    Some((
        PendingFileEvidence {
            ordinal: 0,
            plan,
            before: FileSnapshot::Missing,
            destination_before: None,
        },
        CompletedFileEvidence {
            ordinal: 0,
            after: FileSnapshot::Text(text),
            source_after: None,
        },
    ))
}

pub(crate) fn shell_creation_plan(arguments: &Value) -> Option<FileCapturePlan> {
    parse_creation(arguments).map(|(plan, _)| plan)
}

fn parse_creation(arguments: &Value) -> Option<(FileCapturePlan, String)> {
    let command = arguments.get("cmd")?.as_str()?;
    // Bound parsing and allocations before collecting or decoding any content.
    if command.len() as u64 > MAX_FILE_EVIDENCE_BYTES || command.contains('\r') {
        return None;
    }
    let workdir = Path::new(arguments.get("workdir")?.as_str()?);
    if !workdir.is_absolute() {
        return None;
    }
    let mut lines = command.lines();
    let header = lines.next()?;
    let delimiter = header
        .strip_prefix("python3 -I -S <<'")?
        .strip_suffix('\'')?;
    if delimiter.is_empty()
        || !delimiter
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    let open_line = lines.next()?;
    let path_literal = open_line
        .strip_prefix("with open(")?
        .strip_suffix(", \"x\", encoding=\"utf-8\", newline=\"\") as f:")?;
    let raw_path = python_literal(path_literal)?;
    if raw_path.is_empty() || raw_path.contains(['\0', '\n', '\r']) {
        return None;
    }
    let write_line = lines.next()?;
    let text = python_literal(write_line.strip_prefix("    f.write(")?.strip_suffix(')')?)?;
    if text.contains('\0') || text.len() as u64 > MAX_FILE_EVIDENCE_BYTES {
        return None;
    }
    if lines.next()? != delimiter || lines.next().is_some() {
        return None;
    }
    let path = workdir.join(raw_path);
    Some((
        FileCapturePlan {
            source: FileEvidenceSource::ShellWrite,
            operation: FileOperationHint::Create,
            path_before: path.clone(),
            path_after: path,
        },
        text,
    ))
}

/// Decode only the common subset of JSON and Python double-quoted literals.
/// In particular, JSON's slash and Unicode escapes do not have identical Python
/// semantics (surrogate pairs included). Reject them rather than guessing.
fn python_literal(value: &str) -> Option<String> {
    if !value.starts_with('"') || !value.ends_with('"') {
        return None;
    }
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'\\'
            && !matches!(
                bytes.next()?,
                b'"' | b'\\' | b'n' | b'r' | b't' | b'b' | b'f'
            )
        {
            return None;
        }
    }
    serde_json::from_str(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(path: &str, text: &str) -> String {
        format!(
            "python3 -I -S <<'PY'\nwith open({path}, \"x\", encoding=\"utf-8\", newline=\"\") as f:\n    f.write({text})\nPY"
        )
    }

    fn start(command: &str) -> InvocationStart {
        InvocationStart::new(
            "exec_command",
            json!({"cmd":command,"workdir":"/missing/workdir"}),
        )
    }

    fn success() -> Value {
        json!({"status":"exited","exit_code":0,"termination_reason":"exit"})
    }

    #[test]
    fn derives_exclusive_creation_without_reading_destination() {
        let start = start(&command(r#""new.txt""#, r#""hello\n世界\t\"quoted\"""#));
        let (pending, completed) = completed_shell_creation(&start, &success()).unwrap();
        assert_eq!(pending.before, FileSnapshot::Missing);
        assert_eq!(pending.plan.operation, FileOperationHint::Create);
        assert_eq!(
            pending.plan.path_after,
            Path::new("/missing/workdir/new.txt")
        );
        assert_eq!(
            completed.after,
            FileSnapshot::Text("hello\n世界\t\"quoted\"".into())
        );
        assert_eq!(shell_creation_plan(&start.arguments), Some(pending.plan));
    }

    #[test]
    fn rejects_unsuccessful_or_incomplete_results() {
        let start = start(&command(r#""new.txt""#, r#""hello""#));
        for result in [
            json!({"status":"running","exit_code":0}),
            json!({"status":"exited","exit_code":1}),
            json!({"status":"exited"}),
            json!({"status":"exited","exit_code":0,"termination_reason":"killed"}),
        ] {
            assert!(completed_shell_creation(&start, &result).is_none());
        }
    }

    #[test]
    fn refuses_overwrites_dynamic_code_and_additional_commands() {
        let safe = command(r#""new.txt""#, r#""hello""#);
        for command in [
            "printf hello > new.txt".into(),
            "set -C; printf hello > new.txt".into(),
            "cat > new.txt <<'EOF'\nhello\nEOF".into(),
            safe.replace("\"x\"", "\"w\""),
            safe.replace("-I -S ", ""),
            safe.replace("<<'PY'", "<<PY"),
            safe.replace("f.write(\"hello\")", "f.write(get_content())"),
            safe.replace(
                "f.write(\"hello\")",
                "f.write(\"hello\"); f.write(\"extra\")",
            ),
            format!("{safe}\ntrue"),
            safe.replace("new.txt", "new\\u002etxt"),
        ] {
            assert!(
                completed_shell_creation(&start(&command), &success()).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn limits_size_and_requires_absolute_workdir() {
        let huge = command(
            r#""new.txt""#,
            &format!("\"{}\"", "x".repeat(MAX_FILE_EVIDENCE_BYTES as usize)),
        );
        assert!(completed_shell_creation(&start(&huge), &success()).is_none());
        let mut start = start(&command(r#""new.txt""#, r#""""#));
        start.arguments["workdir"] = json!("relative");
        assert!(completed_shell_creation(&start, &success()).is_none());
    }

    #[test]
    fn literal_decoder_rejects_python_json_differences() {
        for literal in [
            r#""\uD83D\uDE00""#,
            r#""a\/b""#,
            r#""\x41""#,
            r#"f"value""#,
            r#""a" "b""#,
        ] {
            assert!(python_literal(literal).is_none(), "{literal}");
        }
        assert_eq!(python_literal(r#""a\\b\n""#), Some("a\\b\n".into()));
    }
}
