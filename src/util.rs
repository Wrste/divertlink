//! Small utilities: byte-size parsing, tokens, backoff, redaction.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// A byte count that accepts human friendly strings (`64MiB`, `128MB`, `1048576`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub usize);

impl ByteSize {
    /// Raw byte count.
    pub fn bytes(&self) -> usize {
        self.0
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.0;
        const UNITS: [(&str, usize); 4] = [
            ("GiB", 1024 * 1024 * 1024),
            ("MiB", 1024 * 1024),
            ("KiB", 1024),
            ("B", 1),
        ];
        for (name, factor) in UNITS {
            if n != 0 && n.is_multiple_of(factor) {
                return write!(f, "{} {name}", n / factor);
            }
        }
        write!(f, "{n} B")
    }
}

impl FromStr for ByteSize {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Err(Error::Config("empty byte size".to_string()));
        }
        let split = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let num: f64 = num
            .parse()
            .map_err(|_| Error::Config(format!("invalid byte size `{s}`")))?;
        if num < 0.0 {
            return Err(Error::Config(format!("negative byte size `{s}`")));
        }
        let unit = unit.trim().to_ascii_lowercase();
        let factor: f64 = match unit.as_str() {
            "" | "b" | "byte" | "bytes" => 1.0,
            "k" | "kb" => 1000.0,
            "ki" | "kib" => 1024.0,
            "m" | "mb" => 1_000_000.0,
            "mi" | "mib" => 1_048_576.0,
            "g" | "gb" => 1_000_000_000.0,
            "gi" | "gib" => 1_073_741_824.0,
            "t" | "tb" => 1_000_000_000_000.0,
            "ti" | "tib" => 1_099_511_627_776.0,
            other => {
                return Err(Error::Config(format!(
                    "unknown byte size unit `{other}` (use B/KiB/MiB/GiB)"
                )))
            }
        };
        let bytes = num * factor;
        if !bytes.is_finite() || bytes > usize::MAX as f64 {
            return Err(Error::Config(format!("byte size `{s}` is too large")));
        }
        Ok(ByteSize(bytes as usize))
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ByteSizeVisitor;

        impl Visitor<'_> for ByteSizeVisitor {
            type Value = ByteSize;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a byte size such as \"64MiB\" or an integer number of bytes")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                usize::try_from(v)
                    .map(ByteSize)
                    .map_err(|_| E::custom("byte size does not fit in usize"))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                if v < 0 {
                    return Err(E::custom("negative byte size"));
                }
                self.visit_u64(v as u64)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                ByteSize::from_str(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(ByteSizeVisitor)
    }
}

/// Current UNIX time in seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current UNIX time in milliseconds.
pub fn unix_now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Generate `n` random bytes.
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// Lower-case hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Generate a random URL-safe token with an optional human prefix.
///
/// `random_token("dlk_", 24)` yields something like `dlk_5n8...`.
pub fn random_token(prefix: &str, n_bytes: usize) -> String {
    let raw = random_bytes(n_bytes);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    format!("{prefix}{encoded}")
}

/// Generate a random prefixed session identifier.
pub fn session_id() -> String {
    format!("sess_{}", hex(&random_bytes(10)))
}

/// Shorten a secret for logging: never log full tokens or API keys.
pub fn redact(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "***".to_string();
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}…{tail}")
}

/// Exponential backoff with jitter.
///
/// Delays grow as `initial * 2^n`, capped at `max`, with a random multiplier in
/// `[1 - jitter, 1 + jitter]`. This prevents a fleet of agents reconnecting in
/// lockstep after a gateway restart (thundering herd).
#[derive(Debug)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    jitter: f64,
    current: Duration,
    rng: StdRng,
}

impl Backoff {
    /// Create a new backoff schedule.
    pub fn new(initial: Duration, max: Duration, jitter: f64) -> Self {
        let initial = initial.max(Duration::from_millis(1));
        let max = max.max(initial);
        Backoff {
            initial,
            max,
            jitter: jitter.clamp(0.0, 1.0),
            current: initial,
            rng: StdRng::from_entropy(),
        }
    }

    /// Name the next delay and advance the schedule.
    pub fn next_delay(&mut self) -> Duration {
        let base = self.current.as_secs_f64();
        let factor = if self.jitter > 0.0 {
            1.0 + self.rng.gen_range(-self.jitter..=self.jitter)
        } else {
            1.0
        };
        let delay = Duration::from_secs_f64((base * factor).max(0.001));
        self.current = (self.current * 2).min(self.max);
        delay
    }

    /// Reset to the initial delay (call after a healthy session).
    pub fn reset(&mut self) {
        self.current = self.initial;
    }

    /// The maximum delay this schedule can produce (ignoring jitter).
    pub fn max(&self) -> Duration {
        self.max
    }
}

/// Whether a path is a clean absolute HTTP path without traversal segments.
pub fn is_safe_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    // Reject NUL, control characters and raw spaces; hyper already normalizes
    // most of this, but the gateway also receives paths from arbitrary clients.
    if path.bytes().any(|b| b == 0 || b <= b' ') {
        return false;
    }
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => return false,
            _ => {}
        }
    }
    true
}

/// Join a mount prefix and a request path safely.
pub fn join_path(mount: &str, path: &str) -> String {
    let mount = mount.trim_end_matches('/');
    if mount.is_empty() {
        return if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
    }
    if path == "/" || path.is_empty() {
        return format!("{mount}/");
    }
    format!("{mount}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_byte_sizes() {
        assert_eq!(
            "64MiB".parse::<ByteSize>().unwrap().bytes(),
            64 * 1024 * 1024
        );
        assert_eq!("128MB".parse::<ByteSize>().unwrap().bytes(), 128_000_000);
        assert_eq!("1024".parse::<ByteSize>().unwrap().bytes(), 1024);
        assert_eq!("1GiB".parse::<ByteSize>().unwrap().bytes(), 1 << 30);
        assert_eq!("64 KiB".parse::<ByteSize>().unwrap().bytes(), 65536);
        assert!("".parse::<ByteSize>().is_err());
        assert!("12XB".parse::<ByteSize>().is_err());
        assert!("-1MiB".parse::<ByteSize>().is_err());
    }

    #[test]
    fn byte_size_serde() {
        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct Wrapper {
            size: ByteSize,
        }
        let w: Wrapper = toml::from_str("size = \"64MiB\"").unwrap();
        assert_eq!(w.size.bytes(), 64 * 1024 * 1024);
        let w: Wrapper = toml::from_str("size = 4096").unwrap();
        assert_eq!(w.size.bytes(), 4096);
        let rendered = toml::to_string(&w).unwrap();
        assert!(rendered.contains("4 KiB"), "{rendered}");
    }

    #[test]
    fn tokens_are_random_and_redacted() {
        let a = random_token("dlk_", 24);
        let b = random_token("dlk_", 24);
        assert!(a.starts_with("dlk_"));
        assert_ne!(a, b);
        let redacted = redact(&a);
        assert!(!redacted.contains(&a));
        assert_eq!(redact("short"), "***");
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let mut b = Backoff::new(Duration::from_millis(100), Duration::from_secs(2), 0.0);
        assert_eq!(b.next_delay(), Duration::from_millis(100));
        assert_eq!(b.next_delay(), Duration::from_millis(200));
        assert_eq!(b.next_delay(), Duration::from_millis(400));
        assert_eq!(b.next_delay(), Duration::from_millis(800));
        assert_eq!(b.next_delay(), Duration::from_millis(1600));
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_millis(100));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let max = Duration::from_secs(10);
        let mut b = Backoff::new(Duration::from_secs(1), max, 0.3);
        for _ in 0..50 {
            let d = b.next_delay();
            assert!(d <= max.mul_f64(1.3) + Duration::from_millis(5));
            assert!(d >= Duration::from_millis(1));
        }
    }

    #[test]
    fn safe_paths() {
        assert!(is_safe_path("/v1/chat/completions"));
        assert!(is_safe_path("/"));
        assert!(is_safe_path("/a/./b"));
        assert!(!is_safe_path("v1/chat"));
        assert!(!is_safe_path("/../../etc/passwd"));
        assert!(!is_safe_path("/a/../b"));
        assert!(!is_safe_path("/a b"));
    }

    #[test]
    fn path_join() {
        assert_eq!(join_path("/home", "/v1/models"), "/home/v1/models");
        assert_eq!(join_path("/home/", "/v1/models"), "/home/v1/models");
        assert_eq!(join_path("", "/v1/models"), "/v1/models");
        assert_eq!(join_path("/home", "/"), "/home/");
    }
}
