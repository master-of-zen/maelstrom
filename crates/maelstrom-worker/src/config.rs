use bytesize::ByteSize;
use derive_more::From;
use maelstrom_util::config::{BrokerAddr, CacheBytesUsedTarget, CacheRoot, LogLevel};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::{
    fmt::{self, Debug, Formatter},
    path::PathBuf,
};

#[derive(Deserialize)]
#[serde(try_from = "u16")]
pub struct Slots(u16);

impl Slots {
    pub fn inner(&self) -> &u16 {
        &self.0
    }

    pub fn into_inner(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for Slots {
    type Error = String;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value < 1 {
            Err("value must be at least 1".to_string())
        } else if value > 1000 {
            Err("value must be less than 1000".to_string())
        } else {
            Ok(Slots(value))
        }
    }
}

impl Debug for Slots {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Deserialize, From)]
#[serde(from = "u64")]
pub struct InlineLimit(u64);

impl InlineLimit {
    pub fn into_inner(self) -> u64 {
        self.0
    }
}

impl Debug for InlineLimit {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        ByteSize::b(self.0).fmt(f)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Socket address of broker.
    pub broker: BrokerAddr,

    /// The number of job slots available.
    pub slots: Slots,

    /// The directory to use for the cache.
    pub cache_root: CacheRoot,

    /// The target amount of disk space to use for the cache. This bound won't be followed
    /// strictly, so it's best to be conservative.
    pub cache_bytes_used_target: CacheBytesUsedTarget,

    /// The maximum amount of bytes to return inline for captured stdout and stderr.
    pub inline_limit: InlineLimit,

    /// Minimum log level to output.
    pub log_level: LogLevel,
}

#[skip_serializing_none]
#[derive(Serialize)]
pub struct ConfigOptions {
    pub broker: Option<String>,
    pub slots: Option<u16>,
    pub cache_root: Option<PathBuf>,
    pub cache_bytes_used_target: Option<u64>,
    pub inline_limit: Option<u64>,
    pub log_level: Option<LogLevel>,
}

impl Default for ConfigOptions {
    fn default() -> Self {
        ConfigOptions {
            broker: None,
            slots: Some(num_cpus::get().try_into().unwrap()),
            cache_root: Some(".cache/maelstrom-worker".into()),
            cache_bytes_used_target: Some(1_000_000_000),
            inline_limit: Some(1_000_000),
            log_level: Some(LogLevel::Info),
        }
    }
}
