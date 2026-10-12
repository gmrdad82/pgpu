use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

const SETCGROUP: libc::c_short = 0x100;

unsafe extern "C" {
    fn posix_spawnattr_setcgroup_np(
        attr: *mut libc::posix_spawnattr_t,
        cgroup: libc::c_int,
    ) -> libc::c_int;
}

pub(crate) struct Kid {
    pid: u32,
    status: Option<ExitStatus>,
}

impl Kid {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        let mut raw = 0;
        let got = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut raw, libc::WNOHANG) };
        if got < 0 {
            return Err(io::Error::last_os_error());
        }
        if got == 0 {
            return Ok(None);
        }
        self.status = Some(ExitStatus::from_raw(raw));
        Ok(self.status)
    }
}

fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn environment(set: &[(&str, &OsString)]) -> io::Result<Vec<CString>> {
    let mut out = Vec::new();
    for (key, value) in std::env::vars_os() {
        if set
            .iter()
            .any(|(name, _)| key.as_bytes() == name.as_bytes())
        {
            continue;
        }
        let mut pair = key.into_vec();
        pair.push(b'=');
        pair.extend_from_slice(value.as_bytes());
        out.push(c_string(&pair)?);
    }
    for (key, value) in set {
        let mut pair = key.as_bytes().to_vec();
        pair.push(b'=');
        pair.extend_from_slice(value.as_bytes());
        out.push(c_string(&pair)?);
    }
    Ok(out)
}

fn pointers(list: &[CString]) -> Vec<*mut libc::c_char> {
    list.iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect()
}

pub(crate) fn spawn(
    command: &[String],
    set: &[(&str, &OsString)],
    cell: Option<RawFd>,
) -> io::Result<Kid> {
    let argv = command
        .iter()
        .map(|arg| c_string(arg.as_bytes()))
        .collect::<io::Result<Vec<_>>>()?;
    let Some(program) = argv.first() else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    let envp = environment(set)?;
    let argv_p = pointers(&argv);
    let envp_p = pointers(&envp);
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        if libc::posix_spawnattr_init(&mut attr) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut defaults: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut defaults);
        libc::sigaddset(&mut defaults, libc::SIGPIPE);
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        let mut flags =
            (libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK) as libc::c_short;
        let mut code = libc::posix_spawnattr_setsigdefault(&mut attr, &defaults);
        if code == 0 {
            code = libc::posix_spawnattr_setsigmask(&mut attr, &mask);
        }
        if code == 0
            && let Some(fd) = cell
        {
            code = posix_spawnattr_setcgroup_np(&mut attr, fd);
            flags |= SETCGROUP;
        }
        if code == 0 {
            code = libc::posix_spawnattr_setflags(&mut attr, flags);
        }
        let mut pid: libc::pid_t = 0;
        if code == 0 {
            code = libc::posix_spawnp(
                &mut pid,
                program.as_ptr(),
                std::ptr::null(),
                &attr,
                argv_p.as_ptr(),
                envp_p.as_ptr(),
            );
        }
        libc::posix_spawnattr_destroy(&mut attr);
        if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        Ok(Kid {
            pid: pid as u32,
            status: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::spawn;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    fn wait(kid: &mut super::Kid) -> std::process::ExitStatus {
        loop {
            if let Some(status) = kid.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_spawned_command_sees_what_was_set_and_its_status_comes_back() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-spawn");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out");
        let value = OsString::from("yes");
        let mut kid = spawn(
            &[
                "sh".into(),
                "-c".into(),
                format!("echo \"$PGPU_SPAWN_TEST\" > {}; exit 3", out.display()),
            ],
            &[("PGPU_SPAWN_TEST", &value)],
            None,
        )
        .unwrap();
        assert_eq!(wait(&mut kid).code(), Some(3));
        assert_eq!(kid.try_wait().unwrap().unwrap().code(), Some(3));
        assert_eq!(fs::read_to_string(&out).unwrap(), "yes\n");
    }

    #[test]
    fn a_missing_command_is_not_found() {
        let err = spawn(&["pgpu-no-such-command".into()], &[], None)
            .err()
            .unwrap();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }
}
