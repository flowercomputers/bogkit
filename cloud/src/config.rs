use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct Config {
    pub composable_enabled: bool,
    /// Explicit advertised origin for operator-only deployments; never trust request Host.
    pub public_origin: Option<String>,
    /// Roll out separately; disable creation before rollback, retain expiry-capable manager for existing sandboxes.
    pub sandboxes_enabled: bool,
    /// Anonymous, one-hour Bog creation. Existing claimable Bogs still expire when disabled.
    pub claimable_enabled: bool,
    pub claimable_limit: usize,
    pub claimable_per_source_hour: usize,
    pub claimable_daily_limit: usize,
    pub composable_limits: bog_definition::Limits,
    pub root: PathBuf,
    pub worker_binary: PathBuf,
    pub max_active: usize,
    pub max_starts: usize,
    pub min_free_bytes: u64,
    pub idle_timeout: std::time::Duration,
    pub readiness_timeout: std::time::Duration,
}
impl Config {
    pub fn new(root: PathBuf, worker_binary: PathBuf) -> Self {
        Self {
            composable_enabled: false,
            public_origin: None,
            sandboxes_enabled: false,
            claimable_enabled: false,
            claimable_limit: 4,
            claimable_per_source_hour: 6,
            claimable_daily_limit: 24,
            composable_limits: bog_definition::Limits::default(),
            root,
            worker_binary,
            max_active: 8,
            max_starts: 2,
            min_free_bytes: 64 * 1024 * 1024,
            idle_timeout: std::time::Duration::from_secs(600),
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
        if let Ok(value) = std::env::var("BOG_CLOUD_PUBLIC_ORIGIN") {
            config.public_origin = Some(validate_public_origin(&value)?);
        }
        config.sandboxes_enabled = std::env::var("BOG_CLOUD_SANDBOXES").is_ok_and(|v| v == "true");
        config.claimable_enabled = std::env::var("BOG_CLOUD_CLAIMABLE").is_ok_and(|v| v == "true");
        config.claimable_limit = integer("BOG_CLOUD_CLAIMABLE_LIMIT", 4)? as usize;
        config.claimable_per_source_hour =
            integer("BOG_CLOUD_CLAIMABLE_PER_SOURCE_HOUR", 6)? as usize;
        config.claimable_daily_limit = integer("BOG_CLOUD_CLAIMABLE_DAILY_LIMIT", 24)? as usize;
        config.composable_enabled =
            std::env::var("BOG_CLOUD_COMPOSABLE").is_ok_and(|v| v == "true");
        match std::env::var("BOG_CLOUD_COMPOSABLE_LIMITS") {
            Ok(raw) => config.composable_limits = parse_composable_limits(&raw)?,
            Err(std::env::VarError::NotPresent) => {}
            Err(_) => {
                return Err(crate::CloudError::new(
                    "invalid_config",
                    "invalid BOG_CLOUD_COMPOSABLE_LIMITS JSON",
                ));
            }
        }
        config.max_active = integer("BOG_CLOUD_MAX_ACTIVE", 8)? as usize;
        config.max_starts = integer("BOG_CLOUD_MAX_STARTS", 2)? as usize;
        config.min_free_bytes = integer("BOG_CLOUD_MIN_FREE_BYTES", 64 * 1024 * 1024)?;
        if !(1..=64).contains(&config.max_active)
            || !(1..=8).contains(&config.max_starts)
            || config.max_starts > config.max_active
            || config.min_free_bytes < 1024 * 1024
            || !(1..=32).contains(&config.claimable_limit)
            || !(1..=1000).contains(&config.claimable_per_source_hour)
            || !(1..=10000).contains(&config.claimable_daily_limit)
        {
            return Err(crate::CloudError::new(
                "invalid_config",
                "service limits out of range",
            ));
        }
        Ok(config)
    }
}

fn parse_composable_limits(raw: &str) -> Result<bog_definition::Limits, crate::CloudError> {
    let limits: bog_definition::Limits = serde_json::from_str(raw).map_err(|_| {
        crate::CloudError::new("invalid_config", "invalid BOG_CLOUD_COMPOSABLE_LIMITS JSON")
    })?;
    limits.validate().map_err(|_| {
        crate::CloudError::new(
            "invalid_config",
            "composable limits must be positive and cannot exceed hard ceilings",
        )
    })?;
    Ok(limits)
}
#[cfg(test)]
mod limit_tests {
    use super::*;
    #[test]
    fn environment_limits_are_lowering_only_and_strict() {
        let limits =
            parse_composable_limits(r#"{"resources":3,"hits":2,"build_timeout_seconds":10}"#)
                .unwrap();
        assert_eq!(limits.resources, 3);
        assert_eq!(limits.hits, 2);
        assert_eq!(limits.vectors, 10000);
        for raw in [
            "invalid",
            r#"{"resources":17}"#,
            r#"{"hits":0}"#,
            r#"{"unknown":1}"#,
        ] {
            assert_eq!(
                parse_composable_limits(raw).unwrap_err().code,
                "invalid_config"
            );
        }
    }
}

fn validate_public_origin(value: &str) -> Result<String, crate::CloudError> {
    let invalid = || {
        crate::CloudError::new(
            "invalid_config",
            "public origin must be HTTPS or loopback HTTP without a path, query or credentials",
        )
    };
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod origin_tests {
    use super::validate_public_origin;
    #[test]
    fn advertised_origin_is_explicit_and_safe() {
        for value in [
            "https://example.test",
            "http://127.0.0.1:8788",
            "http://[::1]:8788",
        ] {
            assert!(validate_public_origin(value).is_ok(), "{value}");
        }
        for value in [
            "http://example.test",
            "https://user:secret@example.test",
            "https://example.test/path",
            "https://example.test?x",
            "file:///tmp",
        ] {
            assert!(validate_public_origin(value).is_err());
        }
    }
}
