use super::{RemoteFile, human_bytes, run_cli};
use std::{collections::HashSet, io, path::Path};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

struct Entry {
    file_id: u64,
    name: String,
    size: Option<u64>,
}

pub(super) fn literal_path(path: &str) -> Option<String> {
    let relative = path.strip_prefix('/')?;
    if path.chars().any(char::is_control)
        || path.contains('\\')
        || (!relative.is_empty()
            && relative
                .split('/')
                .any(|part| matches!(part, "" | "." | "..")))
    {
        return None;
    }
    // BaiduPCS-Go escapes '[' itself. Escape the other wildcards before passing the path.
    Some(path.replace('*', "\\*").replace('?', "\\?"))
}

fn column(line: &str, start: usize) -> Option<&str> {
    let mut width = 0;
    for (byte, character) in line.char_indices() {
        if width == start {
            return Some(&line[byte..]);
        }
        width += character.width().unwrap_or(0);
        if width > start {
            break;
        }
    }
    None
}

fn parse_directory(output: &str, directory: &str) -> io::Result<Vec<Entry>> {
    let invalid = || io::Error::other("Baidu did not return a complete directory listing");
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    if lines
        .next()
        .and_then(|line| line.strip_prefix("当前目录: "))
        != Some(directory)
        || lines.next() != Some("----")
        || !lines
            .next()
            .is_some_and(|header| header.contains("文件(目录)"))
    {
        return Err(invalid());
    }
    let table = lines
        .take_while(|line| *line != "----" && !line.starts_with("当前目录: "))
        .collect::<Vec<_>>();
    let (counts, rows) = table.split_last().ok_or_else(invalid)?;
    let (prefix, counts) = counts.split_once("文件总数: ").ok_or_else(invalid)?;
    if !prefix.trim_start().starts_with("总: ") {
        return Err(invalid());
    }
    let checksum_column = prefix.width();
    // The detailed footer has seven cells. Its count text occupies the
    // checksum column, immediately before the filename column.
    let mut checksum_width = counts.trim_end().width() + "文件总数: ".width();
    for row in rows {
        let checksum_cell = column(row, checksum_column).ok_or_else(invalid)?;
        if !checksum_cell.starts_with(' ') {
            let value = checksum_cell
                .split_whitespace()
                .next()
                .ok_or_else(invalid)?;
            let md5 = value.strip_prefix("(可能不正确)").unwrap_or(value);
            if !md5.is_empty()
                && (md5.len() != 32 || !md5.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(invalid());
            }
            checksum_width = checksum_width.max(value.width());
        }
    }
    let name_column = checksum_column + checksum_width + 2;
    let (files, dirs) = counts
        .trim()
        .split_once(", 目录总数: ")
        .ok_or_else(invalid)?;
    let files: usize = files.parse().map_err(|_| invalid())?;
    let dirs: usize = dirs.parse().map_err(|_| invalid())?;

    let mut entries = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let mut fields = row.split_whitespace();
        if fields.next().and_then(|value| value.parse().ok()) != Some(index) {
            return Err(invalid());
        }
        let file_id = fields
            .next()
            .and_then(|value| value.parse().ok())
            .filter(|id| *id != 0)
            .ok_or_else(invalid)?;
        let size_field = fields.nth(1).ok_or_else(invalid)?;
        let name = column(row, name_column).ok_or_else(invalid)?.trim();
        let (name, size) = match name.strip_suffix('/') {
            Some(name) if size_field == "-" => (name, None),
            None if size_field != "-" => (name, Some(human_bytes(size_field).ok_or_else(invalid)?)),
            _ => return Err(invalid()),
        };
        if name.is_empty() || name.contains('/') || matches!(name, "." | "..") {
            return Err(invalid());
        }
        entries.push(Entry {
            file_id,
            name: name.to_owned(),
            size,
        });
    }
    if entries.iter().filter(|entry| entry.size.is_some()).count() != files
        || entries.len().checked_sub(files) != Some(dirs)
    {
        return Err(invalid());
    }
    Ok(entries)
}

pub(super) async fn discover(monitor: &str) -> io::Result<Vec<RemoteFile>> {
    let root = Path::new(monitor)
        .components()
        .collect::<std::path::PathBuf>();
    let mut pending = vec![root.to_string_lossy().into_owned()];
    let mut visited = HashSet::new();
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        if !visited.insert(directory.clone()) {
            continue;
        }
        let literal = literal_path(&directory)
            .ok_or_else(|| io::Error::other("Invalid Baidu directory path"))?;
        let output = run_cli(&["ls", "-l", &literal]).await?;
        let entries = match parse_directory(&output, &directory) {
            Ok(entries) => entries,
            Err(error) => {
                // A failed list is not an empty directory. Ask the PCS metadata
                // endpoint to distinguish deletion from a network or parser error.
                if super::cleanup::cloud_file(&directory)
                    .await
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                {
                    continue;
                }
                return Err(error);
            }
        };
        for entry in entries {
            let path = format!("{}/{}", directory.trim_end_matches('/'), entry.name);
            if let Some(size) = entry.size {
                files.push(RemoteFile {
                    id: entry.file_id.to_string(),
                    path,
                    size,
                    local_progress: None,
                });
            } else {
                pending.push(path);
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
#[path = "../../../../../tests/backend/src/worker/baidu/listing.rs"]
mod tests;
