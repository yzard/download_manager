use super::cleanup::RESUME_SUFFIX;
use crate::worker::{LocalProgress, LocalProgressKind};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

const MAX_CHECKPOINT_BYTES: u64 = 65_536;

pub(super) fn read(path: &Path, listed_size: u64) -> Option<LocalProgress> {
    if !path.is_absolute() || fs::canonicalize(path).ok()? != path {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut checkpoint = path.as_os_str().to_owned();
    checkpoint.push(RESUME_SUFFIX);
    let metadata_checkpoint = match fs::symlink_metadata(&checkpoint) {
        Ok(metadata) if metadata.is_file() => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (metadata.len() > 0).then_some(LocalProgress {
                bytes: metadata.len(),
                total: listed_size,
                kind: LocalProgressKind::LocalFile,
            });
        }
        _ => return None,
    };
    if metadata_checkpoint.len() > MAX_CHECKPOINT_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(checkpoint)
        .ok()?
        .take(MAX_CHECKPOINT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return None;
    }
    let (downloaded, total) = checkpoint_progress(&bytes)?;
    // The directory listing rounds values to two decimals in its selected unit.
    if downloaded > metadata.len() || total.abs_diff(listed_size) > total / 200 + 1 {
        return None;
    }
    Some(LocalProgress {
        bytes: downloaded,
        total,
        kind: LocalProgressKind::Checkpoint,
    })
}

// This is the protobuf format written by the pinned BaiduPCS-Go downloader.
fn checkpoint_progress(mut input: &[u8]) -> Option<(u64, u64)> {
    let mut mode = 0;
    let mut total = 0;
    let mut generated = 0;
    let mut ranges = Vec::new();
    while !input.is_empty() {
        match field(&mut input)? {
            (1, Field::Integer(value)) => mode = value,
            (2, Field::Integer(value)) => total = value,
            (3, Field::Integer(value)) => generated = value,
            (5, Field::Bytes(bytes)) => ranges.push(range(bytes)?),
            _ => {}
        }
    }
    let covered = match mode {
        0 => total,
        1 if generated <= total => generated,
        _ => return None,
    };
    if total == 0 || ranges.is_empty() {
        return None;
    }
    ranges.sort_unstable();
    let mut previous_end = 0;
    let mut remaining = 0u64;
    for (begin, end) in ranges {
        if begin < previous_end || end > covered {
            return None;
        }
        remaining = remaining.checked_add(end.checked_sub(begin)?)?;
        previous_end = end;
    }
    Some((covered.checked_sub(remaining)?, total))
}

fn range(mut input: &[u8]) -> Option<(u64, u64)> {
    let mut begin = 0;
    let mut end = 0;
    while !input.is_empty() {
        match field(&mut input)? {
            (1, Field::Integer(value)) => begin = value,
            (2, Field::Integer(value)) => end = value,
            _ => {}
        }
    }
    Some((begin, end))
}

enum Field<'a> {
    Integer(u64),
    Bytes(&'a [u8]),
}

fn field<'a>(input: &mut &'a [u8]) -> Option<(u64, Field<'a>)> {
    let key = varint(input)?;
    let value = match key & 7 {
        0 => Field::Integer(varint(input)?),
        2 => {
            let length = usize::try_from(varint(input)?).ok()?;
            let (value, rest) = input.split_at_checked(length)?;
            *input = rest;
            Field::Bytes(value)
        }
        _ => return None,
    };
    (key >> 3 != 0).then_some((key >> 3, value))
}

fn varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, tail) = input.split_first()?;
        *input = tail;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
#[path = "../../../../../tests/backend/src/worker/baidu/local_progress.rs"]
mod tests;
