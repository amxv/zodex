#[cfg(test)]
mod capture;
mod parse;
mod patch;
mod shell_creation;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

#[cfg(test)]
use crate::invocation::InvocationStart;

pub(crate) use parse::parse_file_capture_plans;
pub(crate) use patch::committed_patch_evidence;
pub(crate) use shell_creation::completed_shell_creation;

pub(crate) const MAX_FILE_EVIDENCE_FILES: usize = 8;
pub(crate) const MAX_FILE_EVIDENCE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileEvidenceSource {
    ApplyPatch,
    ShellWrite,
}

impl FileEvidenceSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ApplyPatch => "apply_patch",
            Self::ShellWrite => "shell_write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileOperationHint {
    Create,
    Update,
    Delete,
    Move,
    Overwrite,
    Append,
}

impl FileOperationHint {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Overwrite => "overwrite",
            Self::Append => "append",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileCapturePlan {
    pub(crate) source: FileEvidenceSource,
    pub(crate) operation: FileOperationHint,
    pub(crate) path_before: PathBuf,
    pub(crate) path_after: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileSnapshot {
    Missing,
    Text(String),
    Unavailable(String),
}

impl FileSnapshot {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Text(_) => "text",
            Self::Unavailable(_) => "unavailable",
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            Self::Missing | Self::Unavailable(_) => None,
        }
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Missing | Self::Text(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingFileEvidence {
    pub(crate) ordinal: u32,
    pub(crate) plan: FileCapturePlan,
    pub(crate) before: FileSnapshot,
    pub(crate) destination_before: Option<FileSnapshot>,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletedFileEvidence {
    pub(crate) ordinal: u32,
    pub(crate) after: FileSnapshot,
    pub(crate) source_after: Option<FileSnapshot>,
}

#[cfg(test)]
pub(crate) fn prepare_file_evidence(start: &InvocationStart) -> Vec<PendingFileEvidence> {
    let Some(plans) = parse_file_capture_plans(start) else {
        return Vec::new();
    };
    plans
        .into_iter()
        .enumerate()
        .map(|(index, plan)| PendingFileEvidence {
            ordinal: index as u32,
            before: capture::capture_text_snapshot(&plan.path_before),
            destination_before: (plan.path_before != plan.path_after)
                .then(|| capture::capture_text_snapshot(&plan.path_after)),
            plan,
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn complete_file_evidence(
    pending: &[PendingFileEvidence],
) -> Vec<CompletedFileEvidence> {
    pending
        .iter()
        .map(|evidence| CompletedFileEvidence {
            ordinal: evidence.ordinal,
            after: capture::capture_text_snapshot(&evidence.plan.path_after),
            source_after: (evidence.plan.path_before != evidence.plan.path_after)
                .then(|| capture::capture_text_snapshot(&evidence.plan.path_before)),
        })
        .collect()
}
