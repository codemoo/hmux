//! Minimal OS bindings missing from the maintained safe wrappers we use.
//! No process enumeration, command execution or retained state lives here.

#[cfg(target_os = "macos")]
pub mod macos {
    use std::io;

    // The sysctl crate requires format metadata for a PID-specific OID. XNU
    // supplies no such metadata for KERN_PROCARGS2; sysctlbyname cannot encode
    // its PID suffix either. Read the fixed numeric MIB into caller-owned bytes.
    #[allow(unsafe_code)]
    fn read<const N: usize>(mut mib: [libc::c_int; N], output: &mut [u8]) -> io::Result<usize> {
        let count = u32::try_from(N)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid sysctl MIB"))?;
        let mut length = output.len();
        // SAFETY: MIB and output are live, exclusively borrowed allocations of
        // exactly the lengths passed. The size pointer refers to a live size_t.
        // The new-value pointer is null/zero (read-only). sysctl copies bytes
        // synchronously and retains none of these pointers. MIBs are private,
        // fixed kernel read operations; no caller-provided pointer is accepted.
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                count,
                output.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length > output.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid sysctl output size",
            ));
        }
        Ok(length)
    }

    fn integer(key: libc::c_int) -> io::Result<i32> {
        let mut bytes = [0; std::mem::size_of::<i32>()];
        if read([libc::CTL_KERN, key], &mut bytes)? != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid kernel integer size",
            ));
        }
        Ok(i32::from_ne_bytes(bytes))
    }

    /// Exact kernel argv/environment bytes. Ownership/identity authorization is
    /// the caller's responsibility; no command data is logged or normalized.
    pub fn process_arguments(pid: i32) -> io::Result<Vec<u8>> {
        if pid <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid process ID",
            ));
        }
        let limit = integer(libc::KERN_ARGMAX)?;
        if !(4..=(2 << 20)).contains(&limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "kernel argument limit exceeds inspection bound",
            ));
        }
        let mut bytes = vec![0; limit as usize];
        let size = read([libc::CTL_KERN, libc::KERN_PROCARGS2, pid], &mut bytes)?;
        bytes.truncate(size);
        Ok(bytes)
    }

    /// Upper bound used before the libproc wrapper allocates a per-user PID list.
    pub fn max_processes_per_user() -> io::Result<i32> {
        let limit = integer(libc::KERN_MAXPROCPERUID)?;
        if !(1..=65_536).contains(&limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "kernel process limit exceeds inspection bound",
            ));
        }
        Ok(limit)
    }
}
