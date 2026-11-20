//! Reversible regression: a service keeps its own bind private while an
//! inherited logical-partition mount disappears when recovery unmounts it.
//! All mounts live in a disposable namespace on tmpfs; no disk is written.
use std::{env, ffi::CString, fs, io, path::Path, process::Command};
fn mount(path: &str, flags: libc::c_ulong) -> io::Result<()> {
    let path = CString::new(path).unwrap();
    let result = unsafe { libc::mount(std::ptr::null(), path.as_ptr(), std::ptr::null(), flags, std::ptr::null()) };
    if result != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().nth(1).as_deref() == Some("child") {
        let mode = env::args().nth(3).unwrap_or_default();
        let old_private = mode == "--old-private";
        if old_private {
            if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 { return Err(io::Error::last_os_error().into()); }
            mount("/", libc::MS_REC | libc::MS_PRIVATE)?;
        } else {
            skyward_block::mount::service_namespace()?;
        }
        let root = env::args().nth(2).unwrap();
        let from = CString::new(format!("{root}/source")).unwrap();
        let to = CString::new(format!("{root}/service-only")).unwrap();
        if unsafe { libc::mount(from.as_ptr(), to.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        fs::write(format!("{root}/ready"), b"ready")?;
        for _ in 0..200 {
            if Path::new(&format!("{root}/check")).exists() {
                let mounts = fs::read_to_string("/proc/self/mountinfo")?;
                let retained = mounts.lines().any(|l| l.split_whitespace().nth(4) == Some(&format!("{root}/partition")));
                let expected_retained = old_private || mode == "--park-before";
                assert_eq!(retained, expected_retained, "unexpected inherited partition mount lifetime");
                assert!(mounts.lines().any(|l| l.split_whitespace().nth(4) == Some(&format!("{root}/service-only"))));
                println!("PASS: mode={mode:?}; retained={retained}; service bind remains isolated");
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        return Err("parent timed out".into());
    }
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 { return Err(io::Error::last_os_error().into()); }
    mount("/", libc::MS_REC | libc::MS_PRIVATE)?;
    let root = format!("/tmp/skyward-namespace-probe-{}", std::process::id());
    fs::create_dir_all(&root)?;
    let root_c = CString::new(root.as_str()).unwrap();
    if unsafe { libc::mount(c"tmpfs".as_ptr(), root_c.as_ptr(), c"tmpfs".as_ptr(), 0, std::ptr::null()) } != 0 { return Err(io::Error::last_os_error().into()); }
    for dir in ["partition", "source", "service-only"] { fs::create_dir(format!("{root}/{dir}"))?; }
    mount(&root, libc::MS_SHARED)?;
    let part = CString::new(format!("{root}/partition")).unwrap();
    if unsafe { libc::mount(c"tmpfs".as_ptr(), part.as_ptr(), c"tmpfs".as_ptr(), 0, std::ptr::null()) } != 0 { return Err(io::Error::last_os_error().into()); }
    let mode = env::args().nth(1).unwrap_or_default();
    let mut child = Command::new(env::current_exe()?).args(["child", &root, &mode]).spawn()?;
    for _ in 0..200 {
        if Path::new(&format!("{root}/ready")).exists() { break; }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(Path::new(&format!("{root}/ready")).exists());
    let mounts = fs::read_to_string("/proc/self/mountinfo")?;
    assert!(!mounts.lines().any(|l| l.split_whitespace().nth(4) == Some(&format!("{root}/service-only"))));
    if mode == "--park-before" {
        mount(&root, libc::MS_PRIVATE)?;
        mount(&root, libc::MS_SHARED)?;
    }
    if unsafe { libc::umount2(part.as_ptr(), 0) } != 0 { return Err(io::Error::last_os_error().into()); }
    if mode == "--park-after" {
        mount(&root, libc::MS_PRIVATE)?;
        mount(&root, libc::MS_SHARED)?;
    }
    fs::write(format!("{root}/check"), b"check")?;
    let status = child.wait()?;
    unsafe { libc::umount2(root_c.as_ptr(), libc::MNT_DETACH); }
    fs::remove_dir(&root)?;
    assert!(status.success(), "child failed: {status}");
    Ok(())
}
