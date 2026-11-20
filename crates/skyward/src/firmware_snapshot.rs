//! Keep the current recovery's firmware independent of the partition it flashes.
//! The transfer runs before ADSP/health/secure services start. New firmware is
//! consumed on the next boot; this recovery continues using its verified copy.
use anyhow::{ensure, Context, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

fn copy_exact(source: &Path, target: &Path, bytes: u64) -> Result<()> {
    ensure!(bytes > 0, "empty firmware partition");
    let mut input = File::open(source)?;
    let mut output = File::options().write(true).create_new(true).open(target)?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut remaining = bytes;
    while remaining != 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..take]).context("short firmware read")?;
        output.write_all(&buffer[..take])?;
        remaining -= take as u64;
    }
    output.sync_all()?;
    drop(output);
    verify_exact(source, target, bytes)
}

fn verify_exact(source: &Path, target: &Path, bytes: u64) -> Result<()> {
    ensure!(fs::metadata(target)?.len() == bytes, "firmware copy length differs");
    let (mut a, mut b) = (File::open(source)?, File::open(target)?);
    let (mut left, mut right) = (vec![0; 1024 * 1024], vec![0; 1024 * 1024]);
    let mut remaining = bytes;
    while remaining != 0 {
        let take = remaining.min(left.len() as u64) as usize;
        a.read_exact(&mut left[..take])?;
        b.read_exact(&mut right[..take])?;
        ensure!(left[..take] == right[..take], "firmware copy verification failed");
        remaining -= take as u64;
    }
    Ok(())
}

fn rooted_copy(root: &Path, firmware_root: &str) -> Result<std::path::PathBuf> {
    use std::path::Component;
    let path = Path::new(firmware_root);
    ensure!(path.is_absolute(), "firmware mount root is not absolute");
    ensure!(path.components().all(|p| matches!(p, Component::RootDir | Component::Normal(_))),
        "unsafe firmware mount root");
    Ok(root.join(path.strip_prefix("/")?))
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use procfs::process::{MountInfo, Process};
    use skyward_block::{fstab, mount};
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
    use std::path::PathBuf;

    const BASE: &str = "/skyward/firmware-memory";
    const BLKROSET: libc::Ioctl = 0x125d;
    const BLKROGET: libc::Ioctl = 0x125e;

    fn entry(partition: &str) -> Result<fstab::Entry> {
        let early = fs::read_to_string("/vendor/etc/charger_fstab.qti").unwrap_or_default();
        if let Some(e) = fstab::entries(&early).into_iter().find(|e|
            Path::new(&e.device).file_name().is_some_and(|n| n == partition)) {
            return Ok(e);
        }
        fstab::partition(partition)?.context("firmware partition absent from fstab")
    }

    fn unheld(device: &Path) -> Result<()> {
        let number = skyward_block::devnum(device)?;
        let namespaces = mount::namespace_holders(number)?;
        ensure!(namespaces.is_empty(), "firmware still mounted: {namespaces:?}");
        let holders = mount::holding(number)?;
        ensure!(holders.is_empty(), "firmware still held: {holders:?}");
        Ok(())
    }

    fn read_only(file: &File) -> Result<i32> {
        let mut value: i32 = -1;
        ensure!(unsafe { libc::ioctl(file.as_raw_fd(), BLKROGET, &mut value) } == 0,
            "read firmware block protection: {}", std::io::Error::last_os_error());
        ensure!(value == 0 || value == 1, "invalid firmware block protection");
        Ok(value)
    }

    fn restore_mounts(old: &[MountInfo], device: &Path, e: &fstab::Entry) -> Result<()> {
        let staging = Path::new(BASE).join("rollback");
        fs::create_dir_all(&staging)?;
        mount::mount(device, &staging, &e.filesystem, &e.options)?;
        let mut paths: Vec<_> = old.iter().collect();
        paths.sort_by_key(|m| m.mount_point.components().count());
        for m in paths {
            if mount::is_mount_point(&m.mount_point)? { continue; }
            mount::bind(&rooted_copy(&staging, &m.root)?, &m.mount_point)?;
        }
        mount::umount(&staging)
    }

    pub fn isolate(partition: &str) -> Result<()> {
        let e = entry(partition)?;
        ensure!(e.options.iter().any(|o| o == "ro"), "firmware must be read-only");
        let device = fs::canonicalize(fstab::device(&e)?)?;
        let base = Path::new(BASE);
        let ready = base.join("ready");
        if ready.exists() { return check(partition); }
        fs::create_dir_all(base)?;
        fs::set_permissions(base, fs::Permissions::from_mode(0o700))?;
        // Rootfs/tmpfs only: no encrypted data, persist or firmware backing.
        let fs_type = mount::visible(Path::new("/"))?.context("no root mount")?.filesystem;
        ensure!(matches!(fs_type.as_str(), "rootfs" | "tmpfs" | "ramfs"), "firmware copy is not in RAM");
        let bytes = skyward_block::size_bytes(&device)?;
        ensure!(bytes <= 512 * 1024 * 1024, "firmware partition exceeds RAM snapshot budget");
        ensure!(mount::space(base)?.available >= bytes + 128 * 1024 * 1024,
            "not enough RAM for firmware snapshot");
        let number = skyward_block::devnum(&device)?;
        let id = format!("{}:{}", number.0, number.1);
        let mut old: Vec<_> = Process::myself()?.mountinfo()?.0.into_iter().filter(|m| m.majmin == id).collect();
        ensure!(!old.is_empty(), "firmware is not mounted before snapshot");
        ensure!(old.iter().all(|m| m.mount_options.contains_key("ro")), "firmware mount is writable");
        ensure!(mount::holding(number)?.is_empty(), "firmware consumers already hold files; isolate before starting services");
        let image = base.join("image");
        // An interrupted startup is not silently reused.
        if image.exists() {
            ensure!(!base.join("loop").exists(), "incomplete firmware snapshot needs a recovery reboot");
            fs::remove_file(&image)?;
        }
        copy_exact(&device, &image, bytes)?;
        fs::set_permissions(&image, fs::Permissions::from_mode(0o400))?;
        let loopdev = skyward_block::loopdev::attach(&image, 0, bytes)?;
        fs::write(base.join("loop"), loopdev.as_os_str().as_encoded_bytes())?;
        let root = base.join("root");
        fs::create_dir_all(&root)?;
        mount::mount(&loopdev, &root, &e.filesystem, &e.options)?;
        for m in &old { ensure!(rooted_copy(&root, &m.root)?.is_dir(), "firmware alias missing in copy"); }
        old.sort_by_key(|m| std::cmp::Reverse((m.mount_point.components().count(), m.mnt_id)));
        let mut removed = Vec::new();
        for m in &old {
            if let Err(error) = mount::umount(&m.mount_point) {
                restore_mounts(&removed, &device, &e).context("restore firmware aliases after failed release")?;
                return Err(error).context("release original firmware; no writes allowed");
            }
            removed.push(m.clone());
        }
        if let Err(error) = unheld(&device) {
            restore_mounts(&removed, &device, &e)?;
            return Err(error);
        }
        old.reverse();
        let mut rebound: Vec<PathBuf> = Vec::new();
        for m in &old {
            if let Err(error) = mount::bind(&rooted_copy(&root, &m.root)?, &m.mount_point) {
                for at in rebound.iter().rev() { mount::umount(at)?; }
                restore_mounts(&old, &device, &e)?;
                return Err(error).context("restore firmware aliases after failed RAM bind");
            }
            rebound.push(m.mount_point.clone());
        }
        unheld(&device)?;
        let original = File::open(&device)?;
        if read_only(&original)? != 0 {
            let writable: i32 = 0;
            ensure!(unsafe { libc::ioctl(original.as_raw_fd(), BLKROSET, &writable) } == 0,
                "release original firmware protection: {}", std::io::Error::last_os_error());
        }
        ensure!(read_only(&original)? == 0, "original firmware remains protected");
        fs::write(&ready, format!("{}\n{}\n{}\n", device.display(), loopdev.display(), bytes))?;
        skyward_sys::kmsg(&format!("firmware: verified {bytes} bytes in RAM on {}; {} released before services start",
            loopdev.display(), device.display()))?;
        check(partition)
    }

    pub fn check(partition: &str) -> Result<()> {
        let e = entry(partition)?;
        let device = fs::canonicalize(fstab::device(&e)?)?;
        let base = Path::new(BASE);
        let record = fs::read_to_string(base.join("ready")).context("firmware is not isolated in RAM; refuse installation")?;
        let fields: Vec<_> = record.lines().collect();
        ensure!(fields.len() == 3 && Path::new(fields[0]) == device, "firmware snapshot identity differs");
        let loopdev = PathBuf::from(fields[1]);
        let loop_id = skyward_block::devnum(&loopdev)?;
        let bytes: u64 = fields[2].parse().context("invalid firmware snapshot length")?;
        ensure!(bytes == skyward_block::size_bytes(&device)? && bytes == fs::metadata(base.join("image"))?.len(),
            "firmware snapshot size differs");
        let loop_name = loopdev.file_name().context("loop device has no name")?;
        let backing = fs::read_to_string(Path::new("/sys/class/block").join(loop_name).join("loop/backing_file"))?;
        ensure!(Path::new(backing.trim()) == base.join("image"), "firmware loop has unexpected backing");
        ensure!(read_only(&File::open(&loopdev)?)? == 1, "firmware RAM loop is not protected");
        let shown = mount::visible(Path::new("/firmware"))?.context("firmware alias missing")?;
        ensure!(shown.device == loop_id, "firmware alias does not use RAM copy");
        unheld(&device)?;
        ensure!(read_only(&File::open(device)?)? == 0, "physical firmware is still read-only");
        Ok(())
    }
}

#[cfg(target_os = "android")]
pub use android::{check, isolate};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_firmware_does_not_change_the_running_recovery_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("partition");
        let snapshot = dir.path().join("ram-copy");
        let old = vec![0x38; 2 * 1024 * 1024 + 13];
        fs::write(&source, &old).unwrap();
        copy_exact(&source, &snapshot, old.len() as u64).unwrap();
        fs::write(&source, vec![0x96; old.len()]).unwrap();
        assert_eq!(fs::read(snapshot).unwrap(), old);
        assert!(verify_exact(&source, &dir.path().join("ram-copy"), old.len() as u64).is_err());
    }
    #[test]
    fn short_copy_never_becomes_ready_and_existing_copy_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source"); let target = dir.path().join("target");
        fs::write(&source, b"abc").unwrap();
        assert!(copy_exact(&source, &target, 4).is_err());
        assert!(copy_exact(&source, &target, 3).is_err());
    }
    #[test]
    fn firmware_bind_subtrees_cannot_escape_the_snapshot() {
        assert_eq!(rooted_copy(Path::new("/ram"), "/image").unwrap(), Path::new("/ram/image"));
        assert_eq!(rooted_copy(Path::new("/ram"), "/").unwrap(), Path::new("/ram"));
        assert!(rooted_copy(Path::new("/ram"), "/../data").is_err());
        assert!(rooted_copy(Path::new("/ram"), "image").is_err());
    }
}
