use super::*;

#[test]
fn reads_sizes_and_names_from_the_v4_detailed_listing() {
    // Produced by the v4.0.2 ls renderer, including the right-aligned numeric name.
    let listing = concat!(
        "\n当前目录: /NAS下载\n----\n",
        "  #  FS ID    APP ID    文件大小       创建日期             修改日期                MD5(截图请打码)             文件(目录)\n",
        "  0    123      250528  1B        1970-01-01 08:00:00  1970-01-01 08:00:00  d41d8cd98f00b204e9800998ecf8427e  音乐  空格.flac\n",
        "  1    456      250528  -         1970-01-01 08:00:00  1970-01-01 08:00:00                                    subdir/\n",
        "  2    789      250528  1.00TB    1970-01-01 08:00:00  1970-01-01 08:00:00                                                 99\n",
        "            总: 1.00TB                                                      文件总数: 2, 目录总数: 1\n",
        "----\n"
    );
    let entries = parse_directory(listing, "/NAS下载").unwrap();
    assert_eq!(entries[0].name, "音乐  空格.flac");
    assert_eq!(entries[0].size, Some(1));
    assert_eq!(entries[1].name, "subdir");
    assert_eq!(entries[1].size, None);
    assert_eq!(entries[2].file_id, 789);
    assert_eq!(entries[2].name, "99");
    assert_eq!(entries[2].size, Some(1 << 40));
    assert!(parse_directory(&listing.replace("文件总数: 2", "文件总数: 3"), "/NAS下载").is_err());
    assert!(parse_directory(listing, "/other").is_err());
    assert!(parse_directory("列目录: 网络错误", "/NAS下载").is_err());
}

#[test]
fn escapes_patterns_without_accepting_path_traversal() {
    assert_eq!(
        literal_path("/NAS下载/[one] *?.zip").as_deref(),
        Some(r"/NAS下载/[one] \*\?.zip")
    );
    assert!(literal_path("/NAS下载/../other").is_none());
    assert!(literal_path("/NAS下载/a\nb").is_none());
    assert!(literal_path("/NAS下载/a\\b").is_none());
}

#[test]
fn compares_exact_sizes_at_directory_listing_precision() {
    assert_eq!(Some(displayed_size(230_133_000)), human_bytes("219.47MB"));
    assert_eq!(Some(displayed_size(1_048_575)), human_bytes("1024.00KB"));
    assert_eq!(displayed_size(1_048_575), displayed_size(1_048_576));
    assert_ne!(displayed_size(230_133_000), displayed_size(231_133_000));
    assert_eq!(displayed_size(1023), 1023);
}
