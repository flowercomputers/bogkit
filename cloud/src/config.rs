use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub worker_binary: PathBuf,
    pub max_active: usize,
    pub max_starts: usize,
    pub min_free_bytes: u64,
    pub readiness_timeout: std::time::Duration,
}
impl Config {
    pub fn new(root: PathBuf, worker_binary: PathBuf) -> Self {
        Self {
            root,
            worker_binary,
            max_active: 8,
            max_starts: 2,
            min_free_bytes: 64 * 1024 * 1024,
            readiness_timeout: std::time::Duration::from_secs(15),
        }
    }
}

/// Check usable filesystem capacity before provisioning/copying, without filling disk.
pub fn require_free_space(root: &std::path::Path, needed: u64) -> Result<(), crate::CloudError> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.as_os_str().as_bytes())
        .map_err(|_| crate::CloudError::new("invalid_config", "invalid storage path"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(crate::CloudError::new(
            "unavailable",
            "cannot inspect storage capacity",
        ));
    }
    let stat = unsafe { stat.assume_init() };
    let available = (stat.f_bavail as u64).saturating_mul(stat.f_frsize);
    if available < needed {
        return Err(crate::CloudError::new(
            "capacity",
            "insufficient storage reserve",
        ));
    }
    Ok(())
}

impl Config {
    pub fn from_env(root: PathBuf, worker_binary: PathBuf) -> Result<Self, crate::CloudError> {
        let mut config = Self::new(root, worker_binary);
        fn integer(name: &str, default: u64) -> Result<u64, crate::CloudError> {
            match std::env::var(name) {
                Ok(value) => value.parse().map_err(|_| {
                    crate::CloudError::new("invalid_config", "invalid numeric service limit")
                }),
                Err(std::env::VarError::NotPresent) => Ok(default),
                Err(_) => Err(crate::CloudError::new(
                    "invalid_config",
                    "invalid service limit",
                )),
            }
        }
        config.max_active = integer("BOG_CLOUD_MAX_ACTIVE", 8)? as usize;
        config.max_starts = integer("BOG_CLOUD_MAX_STARTS", 2)? as usize;
        config.min_free_bytes = integer("BOG_CLOUD_MIN_FREE_BYTES", 64 * 1024 * 1024)?;
        if !(1..=64).contains(&config.max_active)
            || !(1..=8).contains(&config.max_starts)
            || config.max_starts > config.max_active
            || config.min_free_bytes < 1024 * 1024
        {
            return Err(crate::CloudError::new(
                "invalid_config",
                "service limits out of range",
            ));
        }
        Ok(config)
    }
}
