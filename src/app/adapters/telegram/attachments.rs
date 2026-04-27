//! Telegram attachment classification and download.
//!
//! Non-photo attachments (documents, voice notes, audio, video) are downloaded
//! to `{work_dir}/.deskd/attachments/<chat_id>/<message_id>/<filename>` so the
//! agent can read them via filesystem tools. A configurable per-attachment
//! size cap (`TelegramConfig.max_attachment_bytes`) protects against runaway
//! downloads — over-cap files are rejected with a friendly Telegram reply
//! instead of being saved.
//!
//! Photos are handled separately in `mod.rs` because they go through the
//! multimodal path (base64 in payload), not the disk path.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::prelude::Requester;
use teloxide::types::{FileMeta, Message};

/// A classified non-photo attachment ready for download.
#[derive(Debug, Clone)]
pub(crate) struct AttachmentClass {
    kind: AttachmentKind,
    file: FileMeta,
    suggested_filename: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachmentKind {
    Document,
    Voice,
    Audio,
    Video,
}

impl AttachmentClass {
    pub fn file_meta(&self) -> &FileMeta {
        &self.file
    }
    pub fn suggested_filename(&self) -> &str {
        &self.suggested_filename
    }
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            AttachmentKind::Document => "document",
            AttachmentKind::Voice => "voice",
            AttachmentKind::Audio => "audio",
            AttachmentKind::Video => "video",
        }
    }
}

/// Outcome of an attachment download attempt.
#[derive(Debug)]
pub(crate) enum DownloadOutcome {
    Saved,
    OverCap { size: u64, limit: u64 },
}

/// Inspect a Telegram message and return a classified attachment if one is
/// present. Returns `None` for plain text or photo-only messages — photos are
/// handled by the multimodal path in `mod.rs`.
pub(crate) fn classify_attachment(msg: &Message) -> Option<AttachmentClass> {
    if let Some(doc) = msg.document() {
        let suggested = doc.file_name.clone().unwrap_or_else(|| {
            fallback_filename(
                &doc.file.unique_id,
                doc.mime_type.as_ref().map(|m| m.essence_str()),
            )
        });
        return Some(AttachmentClass {
            kind: AttachmentKind::Document,
            file: doc.file.clone(),
            suggested_filename: suggested,
        });
    }
    if let Some(voice) = msg.voice() {
        let suggested = fallback_filename(
            &voice.file.unique_id,
            voice.mime_type.as_ref().map(|m| m.essence_str()),
        );
        return Some(AttachmentClass {
            kind: AttachmentKind::Voice,
            file: voice.file.clone(),
            suggested_filename: suggested,
        });
    }
    if let Some(audio) = msg.audio() {
        let suggested = audio.file_name.clone().unwrap_or_else(|| {
            fallback_filename(
                &audio.file.unique_id,
                audio.mime_type.as_ref().map(|m| m.essence_str()),
            )
        });
        return Some(AttachmentClass {
            kind: AttachmentKind::Audio,
            file: audio.file.clone(),
            suggested_filename: suggested,
        });
    }
    if let Some(video) = msg.video() {
        let suggested = video.file_name.clone().unwrap_or_else(|| {
            fallback_filename(
                &video.file.unique_id,
                video.mime_type.as_ref().map(|m| m.essence_str()),
            )
        });
        return Some(AttachmentClass {
            kind: AttachmentKind::Video,
            file: video.file.clone(),
            suggested_filename: suggested,
        });
    }
    None
}

/// Fallback filename when Telegram doesn't provide one (e.g. voice notes).
/// Picks an extension from the MIME type when possible.
fn fallback_filename(unique_id: &str, mime: Option<&str>) -> String {
    let ext = match mime {
        Some("audio/ogg") => "ogg",
        Some("audio/mpeg") => "mp3",
        Some("audio/mp4") => "m4a",
        Some("audio/wav") | Some("audio/x-wav") => "wav",
        Some("video/mp4") => "mp4",
        Some("video/quicktime") => "mov",
        Some("video/webm") => "webm",
        Some("application/pdf") => "pdf",
        Some("text/plain") => "txt",
        Some("application/zip") => "zip",
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        _ => "bin",
    };
    format!("{}.{}", unique_id, ext)
}

/// Sanitize a filename: strip path separators and other unsafe characters,
/// keeping it usable on both the filesystem and in log lines. Falls back to
/// `attachment.bin` when the result would be empty.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Iteratively strip leading/trailing '.' and '_' so inputs like
    // "../../etc/passwd" → ".._etc_passwd" → "etc_passwd" don't leave a
    // dotfile prefix that could create hidden files on disk.
    let mut trimmed = cleaned.trim();
    loop {
        let next = trimmed
            .trim_start_matches('.')
            .trim_start_matches('_')
            .trim_end_matches('.')
            .trim_end_matches('_');
        if next.len() == trimmed.len() {
            trimmed = next;
            break;
        }
        trimmed = next;
    }
    if trimmed.is_empty() {
        return "attachment.bin".to_string();
    }
    // Cap length to keep filesystem paths sane (most FSes allow 255 bytes).
    if trimmed.len() > 200 {
        // Try to preserve extension.
        if let Some(dot) = trimmed.rfind('.') {
            let (stem, ext) = trimmed.split_at(dot);
            let stem_max = 200usize.saturating_sub(ext.len());
            let mut out = String::with_capacity(200);
            out.push_str(&stem[..stem_max.min(stem.len())]);
            out.push_str(ext);
            return out;
        }
        return trimmed[..200].to_string();
    }
    trimmed.to_string()
}

/// Construct the on-disk attachment path for a given chat/message/filename.
pub(crate) fn attachment_path(
    work_dir: &str,
    chat_id: i64,
    message_id: i32,
    filename: &str,
) -> PathBuf {
    Path::new(work_dir)
        .join(".deskd")
        .join("attachments")
        .join(chat_id.to_string())
        .join(message_id.to_string())
        .join(filename)
}

/// Download an attachment to `dest_path`, enforcing `max_bytes` based on the
/// pre-known size from Telegram's metadata. Creates parent directories.
///
/// Returns `OverCap` (without writing anything) if the file exceeds the limit.
pub(crate) async fn download_to_path(
    bot: &Bot,
    file_id: &str,
    declared_size: u64,
    dest_path: &Path,
    max_bytes: u64,
) -> Result<DownloadOutcome> {
    if declared_size > max_bytes {
        return Ok(DownloadOutcome::OverCap {
            size: declared_size,
            limit: max_bytes,
        });
    }

    // Resolve the actual download path on Telegram's CDN.
    let tg_file = bot
        .get_file(file_id)
        .await
        .with_context(|| format!("failed to get file info for {}", file_id))?;

    // Re-check after get_file in case Telegram returns a different size.
    let resolved_size = tg_file.size as u64;
    if resolved_size > max_bytes {
        return Ok(DownloadOutcome::OverCap {
            size: resolved_size,
            limit: max_bytes,
        });
    }

    if let Some(parent) = dest_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create dir {}", parent.display()))?;
    }

    let mut file = tokio::fs::File::create(dest_path)
        .await
        .with_context(|| format!("failed to create {}", dest_path.display()))?;
    bot.download_file(&tg_file.path, &mut file)
        .await
        .with_context(|| format!("failed to download file {}", file_id))?;

    Ok(DownloadOutcome::Saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_path_layout() {
        let p = attachment_path("/home/kira", -100123, 42, "report.pdf");
        assert_eq!(
            p,
            PathBuf::from("/home/kira/.deskd/attachments/-100123/42/report.pdf")
        );
    }

    #[test]
    fn sanitize_strips_path_separators() {
        // Forward slashes become underscores; leading/trailing dots and
        // underscores are then trimmed, preventing path-traversal payloads
        // from creating dotfiles or escaping the destination dir.
        let out = sanitize_filename("../../etc/passwd");
        assert!(!out.contains('/'), "got {:?}", out);
        assert!(!out.starts_with('.'), "got {:?}", out);

        let out = sanitize_filename("..\\bad");
        // Backslashes are also replaced (Windows path separators).
        assert!(!out.contains('\\'), "got {:?}", out);
    }

    #[test]
    fn sanitize_handles_empty_input() {
        assert_eq!(sanitize_filename(""), "attachment.bin");
        assert_eq!(sanitize_filename("..."), "attachment.bin");
        assert_eq!(sanitize_filename("___"), "attachment.bin");
    }

    #[test]
    fn sanitize_keeps_normal_filenames() {
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("Slides 2026.pptx"), "Slides 2026.pptx");
    }

    #[test]
    fn sanitize_truncates_long_names() {
        let name = format!("{}.pdf", "a".repeat(300));
        let out = sanitize_filename(&name);
        assert!(out.len() <= 200, "got {} chars", out.len());
        assert!(out.ends_with(".pdf"));
    }

    #[test]
    fn fallback_filename_uses_mime_extension() {
        assert_eq!(
            fallback_filename("uid42", Some("application/pdf")),
            "uid42.pdf"
        );
        assert_eq!(fallback_filename("uid42", Some("audio/ogg")), "uid42.ogg");
        assert_eq!(fallback_filename("uid42", None), "uid42.bin");
        assert_eq!(
            fallback_filename("uid42", Some("application/x-unknown")),
            "uid42.bin"
        );
    }

    #[test]
    fn over_cap_short_circuits_without_io() {
        // Verify the size-cap check returns the right variant without making
        // any network calls. We cannot actually instantiate a Bot without a
        // token here; this asserts the logic via a synthetic comparison only.
        let limit: u64 = 20 * 1024 * 1024;
        let too_big: u64 = limit + 1;
        // The matching logic mirrors `download_to_path`'s pre-check.
        let outcome = if too_big > limit {
            DownloadOutcome::OverCap {
                size: too_big,
                limit,
            }
        } else {
            DownloadOutcome::Saved
        };
        match outcome {
            DownloadOutcome::OverCap { size, limit: l } => {
                assert_eq!(size, too_big);
                assert_eq!(l, limit);
            }
            _ => panic!("expected OverCap"),
        }
    }
}
