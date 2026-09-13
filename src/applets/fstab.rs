//! /etc/fstab 解析共享工具（init 挂载与 `mount` 命令共用）。

/// 一条 fstab 挂载记录：`<device> <mountpoint> <type> <options> [<dump> <pass>]`。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FstabEntry {
    pub(crate) device: String,
    pub(crate) mountpoint: String,
    pub(crate) fstype: String,
    pub(crate) options: String,
}

/// 解析一行 fstab 记录；空行、注释行、字段不足的行返回 None。
pub(crate) fn parse_fstab_line(line: &str) -> Option<FstabEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let device = fields.next()?;
    let mountpoint = fields.next()?;
    let fstype = fields.next()?;
    let options = fields.next().unwrap_or("defaults");
    Some(FstabEntry {
        device: device.to_string(),
        mountpoint: mountpoint.to_string(),
        fstype: fstype.to_string(),
        options: options.to_string(),
    })
}

/// 解析整个 fstab 内容。
pub(crate) fn parse_fstab(content: &str) -> Vec<FstabEntry> {
    content.lines().filter_map(parse_fstab_line).collect()
}

/// 按挂载点或设备查找条目（先精确挂载点，再设备）。
pub(crate) fn find_entry<'a>(entries: &'a [FstabEntry], key: &str) -> Option<&'a FstabEntry> {
    entries
        .iter()
        .find(|e| e.mountpoint == key)
        .or_else(|| entries.iter().find(|e| e.device == key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSTAB: &str = "\
# comment

proc     /proc      proc      defaults  0 0
/dev/vda /          ext4      rw,noatime 0 1
tmpfs    /tmp       tmpfs     size=64m  0 0
bad line
";

    #[test]
    fn parses_entries_and_skips_bad_lines() {
        let entries = parse_fstab(FSTAB);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].device, "proc");
        assert_eq!(entries[0].mountpoint, "/proc");
        assert_eq!(entries[0].fstype, "proc");
        assert_eq!(entries[0].options, "defaults");
        assert_eq!(entries[1].options, "rw,noatime");
        assert_eq!(entries[2].options, "size=64m");
    }

    #[test]
    fn finds_by_mountpoint_and_device() {
        let entries = parse_fstab(FSTAB);
        assert_eq!(find_entry(&entries, "/tmp").unwrap().fstype, "tmpfs");
        assert_eq!(find_entry(&entries, "/dev/vda").unwrap().mountpoint, "/");
        assert_eq!(find_entry(&entries, "/nope"), None);
    }

    #[test]
    fn defaults_options_when_missing() {
        let e = parse_fstab_line("tmpfs /tmp tmpfs").unwrap();
        assert_eq!(e.options, "defaults");
    }
}
