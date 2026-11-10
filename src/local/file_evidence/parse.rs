use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::invocation::InvocationStart;

use super::{FileCapturePlan, FileEvidenceSource, FileOperationHint, MAX_FILE_EVIDENCE_FILES};

const ADD_FILE_PREFIX: &str = "*** Add File: ";
const UPDATE_FILE_PREFIX: &str = "*** Update File: ";
const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
const MOVE_TO_PREFIX: &str = "*** Move to: ";

pub(crate) fn parse_file_capture_plans(start: &InvocationStart) -> Option<Vec<FileCapturePlan>> {
    match start.tool_name.as_ref() {
        "apply_patch" => parse_apply_patch(&start.arguments),
        "exec_command" => super::shell_creation::shell_creation_plan(&start.arguments)
            .or_else(|| parse_shell_write(&start.arguments))
            .map(|plan| vec![plan]),
        _ => None,
    }
}

fn parse_apply_patch(arguments: &Value) -> Option<Vec<FileCapturePlan>> {
    let patch = arguments.get("patch")?.as_str()?;
    let workdir = arguments.get("workdir")?.as_str()?;
    let workdir = absolute_workdir(workdir)?;
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch")
        || lines.last().copied() != Some("*** End Patch")
    {
        return None;
    }

    let mut plans = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(raw_path) = line.strip_prefix(ADD_FILE_PREFIX) {
            let path = resolve_path(&workdir, raw_path)?;
            plans.push(FileCapturePlan {
                source: FileEvidenceSource::ApplyPatch,
                operation: FileOperationHint::Create,
                path_before: path.clone(),
                path_after: path,
            });
        } else if let Some(raw_path) = line.strip_prefix(DELETE_FILE_PREFIX) {
            let path = resolve_path(&workdir, raw_path)?;
            plans.push(FileCapturePlan {
                source: FileEvidenceSource::ApplyPatch,
                operation: FileOperationHint::Delete,
                path_before: path.clone(),
                path_after: path,
            });
        } else if let Some(raw_path) = line.strip_prefix(UPDATE_FILE_PREFIX) {
            let source = resolve_path(&workdir, raw_path)?;
            if let Some(move_path) = lines
                .get(index + 1)
                .and_then(|line| line.strip_prefix(MOVE_TO_PREFIX))
            {
                let destination = resolve_path(&workdir, move_path)?;
                plans.push(FileCapturePlan {
                    source: FileEvidenceSource::ApplyPatch,
                    operation: FileOperationHint::Move,
                    path_before: source,
                    path_after: destination,
                });
                index += 1;
            } else {
                plans.push(FileCapturePlan {
                    source: FileEvidenceSource::ApplyPatch,
                    operation: FileOperationHint::Update,
                    path_before: source.clone(),
                    path_after: source,
                });
            }
        } else if line.starts_with(MOVE_TO_PREFIX) || unknown_patch_directive(line) {
            return None;
        }

        if plans.len() > MAX_FILE_EVIDENCE_FILES {
            return None;
        }
        index += 1;
    }

    (!plans.is_empty()).then_some(plans)
}

fn unknown_patch_directive(line: &str) -> bool {
    line.starts_with("*** ")
        && line != "*** Begin Patch"
        && line != "*** End Patch"
        && line != "*** End of File"
        && !line.starts_with(ADD_FILE_PREFIX)
        && !line.starts_with(UPDATE_FILE_PREFIX)
        && !line.starts_with(DELETE_FILE_PREFIX)
        && !line.starts_with(MOVE_TO_PREFIX)
}

fn parse_shell_write(arguments: &Value) -> Option<FileCapturePlan> {
    let command = arguments.get("cmd")?.as_str()?;
    let workdir = absolute_workdir(arguments.get("workdir")?.as_str()?)?;
    parse_cat_heredoc(command, &workdir).or_else(|| parse_printf(command, &workdir))
}

fn parse_cat_heredoc(command: &str, workdir: &Path) -> Option<FileCapturePlan> {
    let normalized = command.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    let first = lines.first()?.trim();
    let tokens = tokenize_simple_shell_line(first)?;
    if tokens.len() != 5 || tokens[0].word()? != "cat" {
        return None;
    }

    let (redirect, path, delimiter) = if tokens[1].operator() == Some("<<") {
        (
            tokens[3].redirect_operator()?,
            tokens[4].word()?,
            tokens[2].word()?,
        )
    } else if tokens[3].operator() == Some("<<") {
        (
            tokens[1].redirect_operator()?,
            tokens[2].word()?,
            tokens[4].word()?,
        )
    } else {
        return None;
    };
    if delimiter.is_empty() {
        return None;
    }

    let closing = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (*line == delimiter).then_some(index))?;
    if lines[closing + 1..]
        .iter()
        .any(|line| !line.trim().is_empty())
    {
        return None;
    }

    let path = resolve_path(workdir, path)?;
    Some(FileCapturePlan {
        source: FileEvidenceSource::ShellWrite,
        operation: if redirect == ">>" {
            FileOperationHint::Append
        } else {
            FileOperationHint::Overwrite
        },
        path_before: path.clone(),
        path_after: path,
    })
}

fn parse_printf(command: &str, workdir: &Path) -> Option<FileCapturePlan> {
    if command.contains(['\n', '\r']) {
        return None;
    }
    let tokens = tokenize_simple_shell_line(command.trim())?;
    let (format_index, operator_index, path_index) = match tokens.as_slice() {
        [
            Token::Word(command),
            Token::Word(_),
            Token::Operator(_),
            Token::Word(_),
        ] if command == "printf" => (1, 2, 3),
        [
            Token::Word(command),
            Token::Word(flag),
            Token::Word(_),
            Token::Operator(_),
            Token::Word(_),
        ] if command == "printf" && flag == "--" => (2, 3, 4),
        _ => return None,
    };
    let _format = tokens[format_index].word()?;
    let redirect = tokens[operator_index].redirect_operator()?;
    let path = resolve_path(workdir, tokens[path_index].word()?)?;
    Some(FileCapturePlan {
        source: FileEvidenceSource::ShellWrite,
        operation: if redirect == ">>" {
            FileOperationHint::Append
        } else {
            FileOperationHint::Overwrite
        },
        path_before: path.clone(),
        path_after: path,
    })
}

fn absolute_workdir(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    path.is_absolute().then(|| path.to_path_buf())
}

fn resolve_path(workdir: &Path, value: &str) -> Option<PathBuf> {
    if value.is_empty() || value.contains(['\n', '\r', '\0']) {
        return None;
    }
    let path = Path::new(value);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Operator(String),
}

impl Token {
    fn word(&self) -> Option<&str> {
        match self {
            Self::Word(value) => Some(value),
            Self::Operator(_) => None,
        }
    }

    fn operator(&self) -> Option<&str> {
        match self {
            Self::Operator(value) => Some(value),
            Self::Word(_) => None,
        }
    }

    fn redirect_operator(&self) -> Option<&str> {
        match self.operator()? {
            ">" | ">>" => self.operator(),
            _ => None,
        }
    }
}

fn tokenize_simple_shell_line(line: &str) -> Option<Vec<Token>> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        while chars.get(index).is_some_and(|ch| ch.is_whitespace()) {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        if let Some((operator, consumed)) = shell_operator(&chars, index) {
            tokens.push(Token::Operator(operator.to_string()));
            index += consumed;
            continue;
        }
        if matches!(chars[index], ';' | '|' | '&' | '(' | ')' | '<' | '>') {
            return None;
        }

        let mut word = String::new();
        while index < chars.len() && !chars[index].is_whitespace() {
            if shell_operator(&chars, index).is_some()
                || matches!(chars[index], ';' | '|' | '&' | '(' | ')' | '<' | '>')
            {
                break;
            }
            match chars[index] {
                '\'' => {
                    index += 1;
                    while index < chars.len() && chars[index] != '\'' {
                        word.push(chars[index]);
                        index += 1;
                    }
                    if index >= chars.len() {
                        return None;
                    }
                    index += 1;
                }
                '"' => {
                    index += 1;
                    while index < chars.len() && chars[index] != '"' {
                        if matches!(chars[index], '$' | '`') {
                            return None;
                        }
                        if chars[index] == '\\' {
                            let next = *chars.get(index + 1)?;
                            if matches!(next, '$' | '`' | '"' | '\\') {
                                word.push(next);
                                index += 2;
                                continue;
                            }
                            word.push('\\');
                            index += 1;
                            continue;
                        }
                        word.push(chars[index]);
                        index += 1;
                    }
                    if index >= chars.len() {
                        return None;
                    }
                    index += 1;
                }
                '\\' => {
                    let next = *chars.get(index + 1)?;
                    word.push(next);
                    index += 2;
                }
                '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '#' => return None,
                value => {
                    word.push(value);
                    index += 1;
                }
            }
        }
        if word.is_empty() {
            return None;
        }
        tokens.push(Token::Word(word));
    }
    Some(tokens)
}

fn shell_operator(chars: &[char], index: usize) -> Option<(&'static str, usize)> {
    match (chars.get(index), chars.get(index + 1)) {
        (Some('>'), Some('>')) => Some((">>", 2)),
        (Some('<'), Some('<')) => Some(("<<", 2)),
        (Some('>'), _) => Some((">", 1)),
        _ => None,
    }
}
